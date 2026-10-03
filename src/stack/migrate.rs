//! Upgrading stored stacks whose definitions held secret values.
//!
//! Before secrets were references, a stack definition carried each value
//! base64 under `secrets`. On start, the daemon moves every such value into
//! the stack's org store as `<stack>_<key>` (a `local` secret the stack
//! owns) and rewrites the definition with [`SecretBinding`]s. Running
//! instances are relabelled with the new revision, since their secrets are
//! the same values, so the upgrade rolls nothing. Running it again changes
//! nothing: a definition without values is left alone, and a value already
//! in the store keeps its version.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::secrets::{SecretBinding, owned_name};
use super::{LABEL_REV, StackDef, Store};
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;

/// What one stack's migration did.
#[derive(Debug, Clone, PartialEq)]
pub struct Migrated {
    /// `org/stack`.
    pub stack: String,
    /// The store names the values went to.
    pub secrets: Vec<String>,
    /// Instances moved to the new revision label in place.
    pub relabelled: usize,
}

impl std::fmt::Display for Migrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "migrated stack {}: {} secret value(s) moved into the org's store ({}); {} instance(s) relabelled in place",
            self.stack,
            self.secrets.len(),
            self.secrets.join(", "),
            self.relabelled
        )
    }
}

/// The base64 values a stored definition still carries under `secrets`.
fn legacy_values(v: &Value) -> BTreeMap<String, String> {
    v.get("secrets")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn has_legacy(v: &Value) -> bool {
    !legacy_values(v).is_empty() || v.get("previous").is_some_and(has_legacy)
}

/// Migrate every stored stack. `client` relabels running instances; without
/// one (tests), only state is rewritten. A stack that fails is reported and
/// left as it was; the others carry on.
pub fn run(
    store: &Store,
    secrets: &Secrets,
    client: Option<&Client>,
) -> Vec<std::result::Result<Migrated, String>> {
    let files = match store.files() {
        Ok(f) => f,
        Err(e) => return vec![Err(format!("cannot list stacks: {e}"))],
    };
    let mut out = Vec::new();
    for p in files {
        match migrate_file(store, secrets, client, &p) {
            Ok(Some(m)) => out.push(Ok(m)),
            Ok(None) => {}
            Err(e) => out.push(Err(format!("cannot migrate {}: {e}", p.display()))),
        }
    }
    out
}

fn migrate_file(
    store: &Store,
    secrets: &Secrets,
    client: Option<&Client>,
    path: &Path,
) -> Result<Option<Migrated>> {
    let text = std::fs::read_to_string(path)?;
    let mut v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    if !has_legacy(&v) {
        return Ok(None);
    }
    let name = v["name"]
        .as_str()
        .ok_or_else(|| Error::invalid("no stack name"))?
        .to_string();
    let org = match v.get("org").and_then(Value::as_str) {
        Some(o) => OrgId::new(o)?,
        None => OrgId::default_org(),
    };
    let legacy = legacy_values(&v);

    // The revisions the running instances carry, from the values.
    let mut old_view = v.clone();
    old_view["secrets"] = json!({});
    old_view["previous"] = Value::Null;
    let old_def: StackDef = serde_json::from_value(old_view)?;
    let mut old_revs = BTreeMap::new();
    for svc in old_def.file.services.keys() {
        let rev = old_def.revision_with(svc, &|k| {
            legacy
                .get(k)
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default()
        })?;
        old_revs.insert(svc.clone(), rev);
    }

    let mut stored = Vec::new();
    let mut current: BTreeMap<String, (Vec<u8>, SecretBinding)> = BTreeMap::new();
    for (key, b64) in &legacy {
        let value = crate::rpc::b64_decode(b64)
            .map_err(|_| Error::invalid(format!("secret {key:?}: not base64")))?;
        let n = owned_name(&name, key)?;
        let m = secrets.put(&org, &n, &value)?;
        stored.push(n.clone());
        current.insert(
            key.clone(),
            (
                value,
                SecretBinding {
                    name: n,
                    driver: m.driver,
                    version: m.version,
                    owned: true,
                },
            ),
        );
    }
    v["secrets"] = bindings_json(&current);
    if let Some(prev) = v.get_mut("previous").filter(|p| p.is_object()) {
        // Only the current value of each secret is kept, so a rollback gets
        // today's value; version 0 marks one that differed.
        let pl = legacy_values(prev);
        let mut pb = serde_json::Map::new();
        for (key, b64) in &pl {
            let same = current
                .get(key)
                .filter(|(cur, _)| crate::rpc::b64_decode(b64).is_ok_and(|x| x == *cur));
            let b = match same {
                Some((_, b)) => b.clone(),
                None => SecretBinding {
                    name: owned_name(&name, key)?,
                    driver: crate::secrets::local::DRIVER.into(),
                    version: 0,
                    owned: true,
                },
            };
            pb.insert(key.clone(), serde_json::to_value(b)?);
        }
        if let Some(obj) = prev.get("secrets").and_then(Value::as_object) {
            for (k, x) in obj {
                if !x.is_string() {
                    pb.insert(k.clone(), x.clone());
                }
            }
        }
        prev["secrets"] = Value::Object(pb);
        // Older still has been dropped on every save; never nests deeper.
        prev["previous"] = Value::Null;
    }
    let def: StackDef = serde_json::from_value(v)?;

    let mut relabelled = 0;
    if let Some(c) = client {
        let oc = crate::org::client(c, &org);
        for (svc, old) in &old_revs {
            let new = def.revision(svc)?;
            if *old == new {
                continue;
            }
            for i in super::controller::list_instances(&oc, &def.name, Some(svc))? {
                if i.rev != *old {
                    continue;
                }
                oc.mutate(
                    "PATCH",
                    &format!("/1.0/instances/{}", encode_segment(&i.name)),
                    Some(&json!({"config": {format!("user.{LABEL_REV}"): new}})),
                    &format!("relabel {} to rev {new}", i.name),
                    oc.get_timeouts().other,
                )?;
                relabelled += 1;
            }
        }
    }
    store.save(&def)?;
    Ok(Some(Migrated {
        stack: def.qualified(),
        secrets: stored,
        relabelled,
    }))
}

fn bindings_json(m: &BTreeMap<String, (Vec<u8>, SecretBinding)>) -> Value {
    Value::Object(
        m.iter()
            .map(|(k, (_, b))| (k.clone(), serde_json::to_value(b).unwrap_or(Value::Null)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_file(org: Option<&str>) -> Value {
        let mut v = json!({
            "name": "app",
            "file": {
                "secrets": {"tok": {"environment": "TOK"}, "pem": {"file": "./k.pem"}},
                "services": {
                    "web": {"image": "dev-base", "secrets": ["tok", "pem"]},
                    "plain": {"image": "dev-base"}
                }
            },
            "base_dir": "/srv",
            "secrets": {
                "tok": crate::rpc::b64_encode(b"t0ken"),
                "pem": crate::rpc::b64_encode(b"PEM")
            },
            "deployed_at": 1,
            "deployed_by": "local",
            "previous": {
                "name": "app",
                "file": {"secrets": {"tok": {"environment": "TOK"}}, "services": {"web": {"image": "dev-base", "secrets": ["tok"]}}},
                "base_dir": "/srv",
                "secrets": {"tok": crate::rpc::b64_encode(b"older")},
                "deployed_at": 0
            }
        });
        if let Some(o) = org {
            v["org"] = json!(o);
        }
        v
    }

    #[test]
    fn moves_values_into_the_store_once() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = super::super::secrets::tests_support::store(&dir.path().join("sec"));
        let store = Store::open(dir.path().join("state")).unwrap();
        let alpha = OrgId::new("alpha").unwrap();
        // One stack per org, written as an older isb did.
        std::fs::write(
            dir.path().join("state/stacks/app.json"),
            serde_json::to_string(&legacy_file(None)).unwrap(),
        )
        .unwrap();
        let adir = alpha.dir(store.dir()).join("stacks");
        std::fs::create_dir_all(&adir).unwrap();
        std::fs::write(
            adir.join("app.json"),
            serde_json::to_string(&legacy_file(Some("alpha"))).unwrap(),
        )
        .unwrap();
        // A current-format stack is not touched.
        let current =
            json!({"name": "new", "file": {"services": {}}, "base_dir": "/", "deployed_at": 5});
        std::fs::write(
            dir.path().join("state/stacks/new.json"),
            serde_json::to_string(&current).unwrap(),
        )
        .unwrap();
        // Old files are unreadable as current definitions until migrated.
        assert_eq!(store.load_all().unwrap().len(), 1);

        let old_rev = {
            let v = legacy_file(None);
            let mut view = v.clone();
            view["secrets"] = json!({});
            view["previous"] = Value::Null;
            let d: StackDef = serde_json::from_value(view).unwrap();
            let l = legacy_values(&v);
            d.revision_with("web", &|k| l[k].as_bytes().to_vec())
                .unwrap()
        };

        let r = run(&store, &s, None);
        assert_eq!(r.len(), 2, "{r:?}");
        let lines: Vec<String> = r.iter().map(|m| m.as_ref().unwrap().to_string()).collect();
        assert!(
            lines[0].starts_with("migrated stack alpha/app: 2 secret value(s)"),
            "{lines:?}"
        );
        assert!(lines[1].contains("stack app:"), "{lines:?}");
        for line in &lines {
            assert!(!line.contains("t0ken"), "{line}");
        }

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 3);
        let app = all.iter().find(|d| d.qualified() == "app").unwrap();
        let org = OrgId::default_org();
        assert_eq!(
            app.secrets["tok"],
            SecretBinding {
                name: "app_tok".into(),
                driver: "local".into(),
                version: 1,
                owned: true
            }
        );
        assert_eq!(s.get(&org, "app_tok").unwrap().0, b"t0ken");
        assert_eq!(s.get(&org, "app_pem").unwrap().0, b"PEM");
        assert_eq!(s.get(&alpha, "app_tok").unwrap().0, b"t0ken");
        // The previous deployment's differing value is gone: version 0.
        let prev = app.previous.as_ref().unwrap();
        assert_eq!(prev.secrets["tok"].version, 0);
        // No value is left in the state file.
        let text = std::fs::read_to_string(dir.path().join("state/stacks/app.json")).unwrap();
        assert!(!text.contains(&crate::rpc::b64_encode(b"t0ken")), "{text}");
        assert!(!text.contains(&crate::rpc::b64_encode(b"older")), "{text}");
        // The revision moved (it is relabelled on a live host).
        assert_ne!(app.revision("web").unwrap(), old_rev);

        // Idempotent: nothing to do, nothing bumped.
        assert!(run(&store, &s, None).is_empty());
        assert_eq!(s.version(&org, "app_tok").unwrap(), 1);
        // A crash after the values were stored but before the rewrite: the
        // rerun stores the same values (no new version) and rewrites.
        std::fs::write(
            dir.path().join("state/stacks/app.json"),
            serde_json::to_string(&legacy_file(None)).unwrap(),
        )
        .unwrap();
        let r = run(&store, &s, None);
        assert_eq!(r.len(), 1);
        assert_eq!(s.version(&org, "app_tok").unwrap(), 1);
    }

    #[test]
    fn a_bad_stack_is_reported_and_left() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = super::super::secrets::tests_support::store(&dir.path().join("sec"));
        let store = Store::open(dir.path().join("state")).unwrap();
        let mut v = legacy_file(None);
        v["secrets"]["tok"] = json!("!!not base64!!");
        std::fs::write(
            dir.path().join("state/stacks/app.json"),
            serde_json::to_string(&v).unwrap(),
        )
        .unwrap();
        let r = run(&store, &s, None);
        assert_eq!(r.len(), 1);
        let e = r[0].as_ref().unwrap_err();
        assert!(e.contains("not base64"), "{e}");
        assert!(!e.contains("not base64!!"), "the value is not echoed: {e}");
    }
}
