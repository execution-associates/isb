//! The `onepassword` driver: secrets that live in 1Password, read with the
//! `op` CLI and a service-account token that belongs to the org.
//!
//! A reference is `vault/item/field` (or `vault/item/section/field`), the
//! `op://` path without its scheme; use the item's ID when its title has a
//! `/` in it. The org's token is its `local` secret
//! [`TOKEN_SECRET`]; it reaches `op` through the environment of that one
//! child, never argv, and no other org's token is ever used for it. The
//! driver is read-only: values are managed in 1Password. A secret's version
//! is the 1Password item's version, which moves on every edit, so polling
//! notices rotations.
//!
//! 1Password limits reads per account per day, so polling is grouped: every
//! field of an item shares the item's version, so one `op item get` answers
//! all the references into one item in a round ([`Driver::versions`]). The
//! item it returns (fields and values included) is kept for [`ITEM_TTL`], so
//! the reads that follow a version bump take their values from it instead
//! of asking again per reference and per replica.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Driver, SecretMeta};
use crate::error::{Error, Result};
use crate::org::OrgId;

/// The org's `local` secret holding its 1Password service-account token.
pub const TOKEN_SECRET: &str = "onepassword-token";

/// How long one `op` call may take.
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an item read from 1Password answers again: long enough to cover
/// one polling round and the deliveries it sets off, short enough that a
/// forced refresh or the next round always asks 1Password.
pub const ITEM_TTL: Duration = Duration::from_secs(20);

/// Reads an org's token: `Ok(None)` when the org has none.
pub type TokenSource = Arc<dyn Fn(&OrgId) -> Result<Option<String>> + Send + Sync>;

/// Items read lately, by (org, vault, item): when, and the item.
type ItemCache = HashMap<(String, String, String), (Instant, Arc<Value>)>;

pub struct OnePasswordDriver {
    op: PathBuf,
    token: TokenSource,
    /// Items read lately, by (org, vault, item): when, and the item.
    items: Mutex<ItemCache>,
}

/// A parsed `vault/item/[section/]field` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub vault: String,
    pub item: String,
    pub field: String,
}

impl Reference {
    pub fn parse(r: &str) -> Result<Reference> {
        let r = r.strip_prefix("op://").unwrap_or(r);
        let parts: Vec<&str> = r.split('/').collect();
        if parts.len() < 3 || parts.len() > 4 || parts.iter().any(|p| p.trim().is_empty()) {
            return Err(Error::invalid(format!(
                "1Password reference {r:?}: expected vault/item/field (or vault/item/section/field)"
            )));
        }
        Ok(Reference {
            vault: parts[0].into(),
            item: parts[1].into(),
            field: parts[2..].join("/"),
        })
    }

    fn uri(&self) -> String {
        format!("op://{}/{}/{}", self.vault, self.item, self.field)
    }
}

impl OnePasswordDriver {
    /// `op` from `$ISB_OP_BIN`, else the first `op` on `$PATH`.
    pub fn new(token: TokenSource) -> OnePasswordDriver {
        let op = std::env::var_os("ISB_OP_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("op"));
        OnePasswordDriver {
            op,
            token,
            items: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_binary(mut self, op: impl Into<PathBuf>) -> Self {
        self.op = op.into();
        self
    }

    /// Run `op` with the org's token and nothing else from our environment
    /// but what it needs to find its config and binaries.
    fn op(&self, org: &OrgId, args: &[&str]) -> Result<Vec<u8>> {
        let token = (self.token)(org)?.ok_or_else(|| {
            Error::invalid(format!(
                "org {org} has no 1Password token: store its service-account token as the secret {TOKEN_SECRET:?} (isb secret create {TOKEN_SECRET} --org {org} -)"
            ))
        })?;
        let mut cmd = Command::new(&self.op);
        cmd.args(args)
            .env_clear()
            .env("OP_SERVICE_ACCOUNT_TOKEN", token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for k in ["PATH", "HOME", "XDG_CONFIG_HOME", "TMPDIR"] {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(|e| {
            Error::invalid(format!(
                "cannot run {} for the onepassword driver: {e} (install the 1Password CLI, or set ISB_OP_BIN)",
                self.op.display()
            ))
        })?;
        let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let reader = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = out.read_to_end(&mut b);
            b
        });
        let err_reader = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = err.read_to_end(&mut b);
            b
        });
        let started = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if started.elapsed() > OP_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::invalid(format!(
                    "op {} took longer than {OP_TIMEOUT:?}",
                    args.first().unwrap_or(&"")
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        if !status.success() {
            let msg = String::from_utf8_lossy(&stderr);
            let msg = msg.trim();
            if msg.contains("isn't an item") || msg.contains("not found") || msg.contains("no item")
            {
                return Err(Error::NotFound(format!(
                    "1Password {}",
                    args.last().unwrap_or(&"")
                )));
            }
            return Err(Error::invalid(format!(
                "op {}: {msg}",
                args.first().unwrap_or(&"")
            )));
        }
        Ok(stdout)
    }

    fn item_key(org: &OrgId, r: &Reference) -> (String, String, String) {
        (org.to_string(), r.vault.clone(), r.item.clone())
    }

    /// The item, from 1Password unless it was read within [`ITEM_TTL`].
    fn item(&self, org: &OrgId, r: &Reference) -> Result<Arc<Value>> {
        let k = Self::item_key(org, r);
        if let Some((at, v)) = self.items.lock().unwrap().get(&k) {
            if at.elapsed() < ITEM_TTL {
                return Ok(v.clone());
            }
        }
        self.fetch_item(org, r)
    }

    /// The item, always from 1Password; remembered for [`ITEM_TTL`].
    fn fetch_item(&self, org: &OrgId, r: &Reference) -> Result<Arc<Value>> {
        let out = self.op(
            org,
            &[
                "item", "get", &r.item, "--vault", &r.vault, "--format", "json",
            ],
        )?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| Error::Protocol(format!("op item get: {e}")))?;
        let v = Arc::new(v);
        let mut items = self.items.lock().unwrap();
        items.retain(|_, (at, _)| at.elapsed() < ITEM_TTL);
        items.insert(Self::item_key(org, r), (Instant::now(), v.clone()));
        Ok(v)
    }

    fn meta(&self, org: &OrgId, name: &str, item: &Value) -> SecretMeta {
        let version = item["version"].as_u64().unwrap_or(0);
        let ts = |k: &str| item[k].as_str().and_then(rfc3339_to_unix).unwrap_or(0);
        SecretMeta {
            org: org.clone(),
            name: name.to_string(),
            driver: "onepassword".into(),
            version,
            created_at: ts("created_at"),
            updated_at: ts("updated_at"),
            labels: BTreeMap::new(),
        }
    }
}

/// A field's value in an item as `op item get --format json` prints it:
/// `field` is a field's id or label, `section/field` one in a section (by
/// its id or label). `None` when it is not there exactly once, or has no
/// value (a file, an OTP), so the caller asks `op read`, which knows every
/// form.
fn field_value(item: &Value, field: &str) -> Option<Vec<u8>> {
    let (section, field) = match field.split_once('/') {
        Some((s, f)) => (Some(s), f),
        None => (None, field),
    };
    let named = |v: &Value, n: &str| v["id"].as_str() == Some(n) || v["label"].as_str() == Some(n);
    let hits: Vec<&Value> = item["fields"]
        .as_array()?
        .iter()
        .filter(|f| named(f, field))
        .filter(|f| section.is_none_or(|s| named(&f["section"], s)))
        .collect();
    match hits.as_slice() {
        [f] => f["value"].as_str().map(|v| v.as_bytes().to_vec()),
        _ => None,
    }
}

/// A name that is not a reference at all is simply not this driver's (the
/// facade asks every driver); a malformed reference is an error.
fn reference(name: &str) -> Result<Reference> {
    if !name.contains('/') {
        return Err(Error::NotFound(format!("1Password reference {name}")));
    }
    Reference::parse(name)
}

impl Driver for OnePasswordDriver {
    fn name(&self) -> &str {
        "onepassword"
    }

    fn get(&self, org: &OrgId, name: &str) -> Result<(Vec<u8>, u64)> {
        let r = reference(name)?;
        let item = self.item(org, &r)?;
        let version = item["version"].as_u64().unwrap_or(0);
        let value = match field_value(&item, &r.field) {
            Some(v) => v,
            None => self.op(org, &["read", "--no-newline", &r.uri()])?,
        };
        Ok((value, version))
    }

    /// Polling asks 1Password every time; the answer then serves the reads
    /// a new version sets off.
    fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
        let r = reference(name)?;
        Ok(self.fetch_item(org, &r)?["version"].as_u64().unwrap_or(0))
    }

    /// One `op item get` per (vault, item), whatever the number of fields
    /// referenced in it.
    fn versions(&self, org: &OrgId, names: &[&str]) -> Vec<Result<u64>> {
        let mut round: HashMap<(String, String), std::result::Result<u64, String>> = HashMap::new();
        names
            .iter()
            .map(|name| {
                let r = reference(name)?;
                let got = round
                    .entry((r.vault.clone(), r.item.clone()))
                    .or_insert_with(|| {
                        self.fetch_item(org, &r)
                            .map(|i| i["version"].as_u64().unwrap_or(0))
                            .map_err(|e| e.to_string())
                    });
                got.clone().map_err(Error::invalid)
            })
            .collect()
    }

    /// Forget what was read lately, so the next look asks 1Password.
    fn refresh(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        let r = reference(name)?;
        let item = self.fetch_item(org, &r)?;
        Ok(self.meta(org, name, &item))
    }

    fn inspect(&self, org: &OrgId, name: &str) -> Result<SecretMeta> {
        let r = reference(name)?;
        let item = self.item(org, &r)?;
        Ok(self.meta(org, name, &item))
    }

    /// A vault is not this org's to enumerate: references are listed where
    /// they are used (stacks), not here.
    fn list(&self, _org: &OrgId) -> Result<Vec<SecretMeta>> {
        Ok(Vec::new())
    }
}

/// `2026-10-03T05:12:11Z` (or with an offset) as unix seconds.
fn rfc3339_to_unix(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time: String = rest.chars().take(8).collect();
    let mut t = time.split(':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let tail = &rest[8.min(rest.len())..];
    let tail = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match tail {
        "" | "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let o = o.trim_start_matches(['+', '-']);
            let (oh, om) = o.split_once(':').unwrap_or((o, "0"));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
    };
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss - offset;
    u64::try_from(secs).ok()
}

/// A fake `op` that logs every call and serves items from a directory:
/// `<dir>/items/<vault>.<item>` holds the item's version (a missing file is
/// an unknown item). Each item has fields `password` and `login/user`.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    pub(crate) struct FakeOp {
        pub dir: tempfile::TempDir,
    }

    impl FakeOp {
        pub(crate) fn new() -> FakeOp {
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path().display();
            std::fs::create_dir(dir.path().join("items")).unwrap();
            let script = format!(
                r#"#!/bin/sh
echo "$*" >> {d}/calls
case "$1 $2" in
  "item get")
    f="{d}/items/$5.$3"
    [ -f "$f" ] || {{ echo "\"$3\" isn't an item" >&2; exit 1; }}
    printf '{{"version": %s, "fields": [{{"id": "password", "label": "password", "value": "pw-%s-%s"}}, {{"id": "u1", "label": "user", "section": {{"id": "s1", "label": "login"}}, "value": "user-%s"}}]}}' "$(cat "$f")" "$3" "$(cat "$f")" "$3"
    ;;
  "read --no-newline") printf 'read:%s' "$3" ;;
  *) exit 2 ;;
esac
"#
            );
            let op = dir.path().join("op");
            std::fs::write(&op, script).unwrap();
            std::fs::set_permissions(&op, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            FakeOp { dir }
        }

        pub(crate) fn driver(&self) -> OnePasswordDriver {
            let token: TokenSource = Arc::new(|_| Ok(Some("tok".to_string())));
            OnePasswordDriver::new(token).with_binary(self.dir.path().join("op"))
        }

        pub(crate) fn set_version(&self, vault: &str, item: &str, v: u64) {
            std::fs::write(
                self.dir
                    .path()
                    .join("items")
                    .join(format!("{vault}.{item}")),
                v.to_string(),
            )
            .unwrap();
        }

        /// Every call so far, one `op` argv per line.
        pub(crate) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let r = Reference::parse("k8s-ocai/smtp/password").unwrap();
        assert_eq!(
            (r.vault.as_str(), r.item.as_str(), r.field.as_str()),
            ("k8s-ocai", "smtp", "password")
        );
        assert_eq!(r.uri(), "op://k8s-ocai/smtp/password");
        assert_eq!(Reference::parse("op://v/i/s/f").unwrap().field, "s/f");
        assert!(Reference::parse("v/i").is_err());
        assert!(Reference::parse("v//f").is_err());
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("2026-10-03T04:00:00Z"), Some(1_791_000_000));
        assert_eq!(
            rfc3339_to_unix("2026-10-03T06:00:00.123+02:00"),
            Some(1_791_000_000)
        );
    }

    /// A fake `op` that checks it got the org's token from the environment
    /// (and nothing from argv), and answers `read` and `item get`.
    #[test]
    fn talks_to_op_with_the_orgs_token() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("op");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             [ \"$OP_SERVICE_ACCOUNT_TOKEN\" = tok-alpha ] || { echo 'bad token' >&2; exit 1; }\n\
             case \"$1\" in\n\
               read) printf 's3cret' ;;\n\
               item) printf '{\"version\": 7, \"updated_at\": \"2026-10-03T04:00:00Z\"}' ;;\n\
               *) exit 2 ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let alpha = OrgId::new("alpha").unwrap();
        let token: TokenSource =
            Arc::new(|o: &OrgId| Ok((o.as_str() == "alpha").then(|| "tok-alpha".to_string())));
        let d = OnePasswordDriver::new(token).with_binary(&fake);
        assert_eq!(d.get(&alpha, "v/i/f").unwrap(), (b"s3cret".to_vec(), 7));
        assert_eq!(d.version(&alpha, "v/i/f").unwrap(), 7);
        assert_eq!(
            d.inspect(&alpha, "v/i/f").unwrap().updated_at,
            1_791_000_000
        );
        assert!(d.list(&alpha).unwrap().is_empty());
        let e = d
            .get(&OrgId::new("beta").unwrap(), "v/i/f")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no 1Password token"), "{e}");
        assert!(d.set(&alpha, "v/i/f", b"x").is_err());
    }

    /// 150 references into 10 items, polled in one round: 10 `op item get`,
    /// not 150. The values a bump then needs come from those answers.
    #[test]
    fn a_polling_round_asks_once_per_item() {
        let op = fake::FakeOp::new();
        for i in 0..10 {
            op.set_version("v", &format!("item{i}"), 3);
        }
        let d = op.driver();
        let org = OrgId::new("alpha").unwrap();
        let refs: Vec<String> = (0..150)
            .map(|n| match n % 3 {
                0 => format!("v/item{}/password", n % 10),
                1 => format!("v/item{}/login/user", n % 10),
                _ => format!("v/item{}/field{n}", n % 10),
            })
            .collect();
        let names: Vec<&str> = refs.iter().map(String::as_str).collect();
        let got = d.versions(&org, &names);
        assert!(got.iter().all(|v| *v.as_ref().unwrap() == 3));
        assert_eq!(op.calls().len(), 10, "{:?}", op.calls());
        assert!(op.calls().iter().all(|c| c.starts_with("item get")));

        // Item 4 moves: the next round still asks once per item, and sees it.
        op.set_version("v", "item4", 4);
        let got = d.versions(&org, &names);
        assert_eq!(op.calls().len(), 20);
        for (r, v) in refs.iter().zip(&got) {
            let want = if r.starts_with("v/item4/") { 4 } else { 3 };
            assert_eq!(*v.as_ref().unwrap(), want, "{r}");
        }

        // Reading the moved references right after (what a roll does, per
        // replica) costs nothing more: the round's answer holds the fields.
        for _ in 0..3 {
            assert_eq!(
                d.get(&org, "v/item4/password").unwrap(),
                (b"pw-item4-4".to_vec(), 4)
            );
            assert_eq!(
                d.get(&org, "v/item4/login/user").unwrap(),
                (b"user-item4".to_vec(), 4)
            );
        }
        assert_eq!(op.calls().len(), 20, "{:?}", op.calls());
        // A field the item's JSON does not carry is read with `op read`.
        assert_eq!(
            d.get(&org, "v/item4/field9").unwrap(),
            (b"read:op://v/item4/field9".to_vec(), 4)
        );
        assert_eq!(op.calls().len(), 21);
        // A forced refresh always asks.
        assert_eq!(d.refresh(&org, "v/item4/password").unwrap().version, 4);
        assert_eq!(op.calls().len(), 22);
        // An unknown item fails its own references only, once.
        let got = d.versions(&org, &["v/ghost/a", "v/ghost/b", "v/item1/password"]);
        assert!(got[0].is_err() && got[1].is_err());
        assert_eq!(*got[2].as_ref().unwrap(), 3);
        assert_eq!(op.calls().len(), 24);
        // Orgs never share an answer.
        let beta = OrgId::new("beta").unwrap();
        d.get(&beta, "v/item4/password").unwrap();
        assert_eq!(op.calls().len(), 25);
    }

    /// The facade finds each name's driver: store names in `local`,
    /// references in 1Password, all of them in one `op` call per item.
    #[test]
    fn the_facade_polls_mixed_names_in_one_round() {
        let op = fake::FakeOp::new();
        op.set_version("v", "a", 5);
        op.set_version("v", "b", 6);
        let dir = tempfile::tempdir().unwrap();
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let s =
            crate::secrets::Secrets::new(crate::secrets::LocalDriver::new(dir.path(), Arc::new(k)))
                .with_driver(Arc::new(op.driver()))
                .unwrap();
        let org = OrgId::default_org();
        s.create(&org, "plain", None, b"x", &BTreeMap::new())
            .unwrap();
        let got = s.versions(
            &org,
            &[
                "v/a/password",
                "plain",
                "v/a/login/user",
                "v/b/password",
                "v/ghost/x",
                "missing",
            ],
        );
        let ok: Vec<Option<u64>> = got.iter().map(|r| r.as_ref().ok().copied()).collect();
        assert_eq!(ok, [Some(5), Some(1), Some(5), Some(6), None, None]);
        assert_eq!(op.calls().len(), 3, "{:?}", op.calls());
    }

    #[test]
    fn field_values_from_the_item() {
        let item: Value = serde_json::json!({"fields": [
            {"id": "password", "label": "password", "value": "pw"},
            {"id": "x1", "label": "user", "section": {"id": "s1", "label": "login"}, "value": "u"},
            {"id": "x2", "label": "user", "section": {"id": "s2", "label": "admin"}, "value": "a"},
            {"id": "doc", "label": "doc"},
        ]});
        assert_eq!(field_value(&item, "password"), Some(b"pw".to_vec()));
        assert_eq!(field_value(&item, "login/user"), Some(b"u".to_vec()));
        assert_eq!(field_value(&item, "s2/x2"), Some(b"a".to_vec()));
        // Ambiguous, valueless or missing: left to `op read`.
        assert_eq!(field_value(&item, "user"), None);
        assert_eq!(field_value(&item, "doc"), None);
        assert_eq!(field_value(&item, "nope"), None);
    }
}
