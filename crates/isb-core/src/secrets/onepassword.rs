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

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Driver, SecretMeta};
use crate::error::{Error, Result};
use crate::org::OrgId;

/// The org's `local` secret holding its 1Password service-account token.
pub const TOKEN_SECRET: &str = "onepassword-token";

/// How long one `op` call may take.
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// Reads an org's token: `Ok(None)` when the org has none.
pub type TokenSource = Arc<dyn Fn(&OrgId) -> Result<Option<String>> + Send + Sync>;

pub struct OnePasswordDriver {
    op: PathBuf,
    token: TokenSource,
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
        OnePasswordDriver { op, token }
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

    fn item(&self, org: &OrgId, r: &Reference) -> Result<Value> {
        let out = self.op(
            org,
            &[
                "item", "get", &r.item, "--vault", &r.vault, "--format", "json",
            ],
        )?;
        serde_json::from_slice(&out).map_err(|e| Error::Protocol(format!("op item get: {e}")))
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
        let value = self.op(org, &["read", "--no-newline", &r.uri()])?;
        let version = self.item(org, &r)?["version"].as_u64().unwrap_or(0);
        Ok((value, version))
    }

    fn version(&self, org: &OrgId, name: &str) -> Result<u64> {
        let r = reference(name)?;
        Ok(self.item(org, &r)?["version"].as_u64().unwrap_or(0))
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
}
