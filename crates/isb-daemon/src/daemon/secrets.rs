//! The `secret_*` tools: the org's secret store over MCP and the unix
//! socket. Values travel base64 in arguments and results.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::server::{Caller, Registry, Tool};
use crate::stack::secrets::Cycle;

type Handler = Box<dyn Fn(&Secrets, Value, &Caller) -> Result<Value> + Send + Sync>;

/// The stacks in an org whose services use a stored secret.
pub type InUse = Arc<dyn Fn(&OrgId, &str) -> Vec<String> + Send + Sync>;

/// What a new secret version did: per stack service, its `on_change`
/// action, and what was not cycled (workspaces) and why.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub cycles: Vec<Cycle>,
    pub skipped: Vec<Skipped>,
}

/// Something using a secret that a new version does not restart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    /// `workspace`.
    pub kind: String,
    pub name: String,
    pub reason: String,
}

impl Outcome {
    /// `rolled`, `services` and `skipped`, as the tools answer them.
    fn fill(&self, v: &mut Value) {
        v["rolled"] = json!(crate::stack::secrets::cycled_stacks(&self.cycles));
        v["services"] = json!(self.cycles);
        v["skipped"] = json!(self.skipped);
    }
}

/// Before a new value is stored: make it take effect where the old one is
/// kept (a stack secret's `rotate`, such as a database's password). An
/// error stops the set.
pub type Prepare =
    Arc<dyn Fn(&OrgId, &str, &[u8]) -> Result<Vec<crate::stack::secrets::Applied>> + Send + Sync>;

/// A stored secret got a new value: cycle what uses it, per its
/// `on_change`.
pub type Changed = Arc<dyn Fn(&OrgId, &str) -> Outcome + Send + Sync>;

/// Re-read every stack reference to a name through its driver now; returns
/// (driver, version) per driver and what the new version did.
pub type Refresh = Arc<dyn Fn(&OrgId, &str) -> Result<(Vec<(String, u64)>, Outcome)> + Send + Sync>;

/// One stack's use of a secret, as deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// A store name, or a driver reference (`vault/item/field`).
    pub name: String,
    pub driver: String,
    pub version: u64,
    pub stack: String,
}

/// Every secret the org's deployed stacks use.
pub type Bindings = Arc<dyn Fn(&OrgId) -> Vec<Binding> + Send + Sync>;

/// How the secret tools reach the stacks.
#[derive(Clone)]
pub struct Hooks {
    pub prepare: Prepare,
    pub in_use: InUse,
    pub changed: Changed,
    pub refresh: Refresh,
    pub bindings: Bindings,
}

impl Hooks {
    /// No stacks at all.
    pub fn none() -> Hooks {
        Hooks {
            prepare: Arc::new(|_, _, _| Ok(Vec::new())),
            in_use: Arc::new(|_, _| Vec::new()),
            changed: Arc::new(|_, _| Outcome::default()),
            refresh: Arc::new(|_, _| Ok((Vec::new(), Outcome::default()))),
            bindings: Arc::new(|_| Vec::new()),
        }
    }
}

fn args<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::invalid(format!("bad arguments: {e}")))
}

/// The org a call acts on.
fn org_for(c: &Caller, org: Option<&str>) -> Result<OrgId> {
    let org = match org {
        Some(o) => OrgId::new(o)?,
        None => OrgId::default_org(),
    };
    // P1.7: bind a remote caller (token or identity) to its own org here and
    // refuse any other. Until then every caller reaches every org.
    let _ = c.is_trusted();
    Ok(org)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    #[serde(default)]
    org: Option<String>,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrgOnly {
    #[serde(default)]
    org: Option<String>,
}

fn props(extra: Value) -> Value {
    let mut p = json!({
        "org": {"type": "string", "description": "The org (default: default)."},
    });
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    p
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn name_prop() -> Value {
    json!({"name": {"type": "string", "description": "Secret name: 1-128 of [A-Za-z0-9_.-], not starting with '.'."}})
}

fn value_of(b64: &str) -> Result<Vec<u8>> {
    crate::rpc::b64_decode(b64).map_err(|_| Error::invalid("value must be base64"))
}

/// `secret_list`'s answer: the stored secrets with the stacks using each,
/// then the references stacks use that are not in the store.
fn listing(stored: Vec<crate::secrets::SecretMeta>, used: Vec<Binding>) -> Result<Value> {
    let stacks_of = |name: &str| -> Vec<String> {
        let mut v: Vec<String> = used
            .iter()
            .filter(|b| b.name == name)
            .map(|b| b.stack.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let mut secrets = Vec::new();
    for m in &stored {
        let mut v = serde_json::to_value(m)?;
        v["used_by"] = json!(stacks_of(&m.name));
        secrets.push(v);
    }
    let mut refs: BTreeMap<&str, Value> = BTreeMap::new();
    for b in used
        .iter()
        .filter(|b| !stored.iter().any(|m| m.name == b.name))
    {
        refs.entry(b.name.as_str()).or_insert_with(|| {
            json!({"name": b.name, "driver": b.driver, "version": b.version, "used_by": stacks_of(&b.name)})
        });
    }
    Ok(json!({"secrets": secrets, "references": refs.into_values().collect::<Vec<_>>()}))
}

/// Register the secret tools.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn register(r: &mut Registry, secrets: Arc<Secrets>, hooks: Hooks) -> Result<()> {
    let Hooks {
        prepare,
        in_use,
        changed,
        refresh,
        bindings,
    } = hooks;
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});
    let value_prop = json!({"value": {"type": "string", "description": "The value, base64."}});

    let mut add = |name: &str,
                   title: &str,
                   desc: &str,
                   schema: Value,
                   ann: &Value,
                   f: Handler|
     -> Result<()> {
        let s = secrets.clone();
        r.register(
            Tool::new(name, desc, schema, move |a, c| f(&s, a, c))
                .title(title)
                .annotations(ann.clone()),
        )
    };

    add(
        "secret_create",
        "Create a secret",
        "Create a secret in an org's store (fails if one by that name exists). The value is base64; it is encrypted at rest and never listed.",
        obj(
            props(json!({
                "name": name_prop()["name"],
                "value": value_prop["value"],
                "driver": {"type": "string", "description": "Where it is stored (default: local)."},
                "labels": {"type": "object", "additionalProperties": {"type": "string"}}
            })),
            &["name", "value"],
        ),
        &write,
        Box::new(|s, a, c| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                org: Option<String>,
                name: String,
                value: String,
                #[serde(default)]
                driver: Option<String>,
                #[serde(default)]
                labels: BTreeMap<String, String>,
            }
            let a: A = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            let m = s.create(
                &org,
                &a.name,
                a.driver.as_deref(),
                &value_of(&a.value)?,
                &a.labels,
            )?;
            Ok(serde_json::to_value(m)?)
        }),
    )?;
    let on_set = changed.clone();
    add(
        "secret_set",
        "Set a secret",
        "Give a secret a new value (base64), bumping its version; creates it in the local store if missing. Each stack service using it acts per its `on_change`: `roll` (default; a rolling update), `restart` (in place) or `none` (files updated, replicas stale until they next start). `services` lists what each service did, `rolled` the stacks that roll or restart, `skipped` what was not cycled and why (workspaces get the file, never a restart). A stack secret with a `rotate` command (a database app's password) is first changed where the old value is kept, in a running replica (`applied`); if that fails, nothing is stored. A database app's URL secret follows its password.",
        obj(
            props(json!({"name": name_prop()["name"], "value": value_prop["value"]})),
            &["name", "value"],
        ),
        &write,
        Box::new(move |s, a, c| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                org: Option<String>,
                name: String,
                value: String,
            }
            let a: A = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            let value = value_of(&a.value)?;
            let applied = prepare(&org, &a.name, &value)?;
            let m = s.set(&org, &a.name, &value)?;
            let outcome = on_set(&org, &a.name);
            let mut v = serde_json::to_value(m)?;
            outcome.fill(&mut v);
            v["applied"] = json!(applied);
            Ok(v)
        }),
    )?;
    add(
        "secret_get",
        "Read a secret",
        "A secret's value (base64, in `value`) and metadata.",
        obj(props(name_prop()), &["name"]),
        &ro,
        Box::new(|s, a, c| {
            let a: Named = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            let (v, m) = s.get(&org, &a.name)?;
            Ok(json!({"meta": m, "value": crate::rpc::b64_encode(&v)}))
        }),
    )?;
    add(
        "secret_list",
        "List secrets",
        "An org's secrets: name, driver, version, timestamps, labels, and the deployed stacks using each (`used_by`). Never values. `references` lists the driver references (such as 1Password's vault/item/field) stacks use, which live outside the store.",
        obj(props(json!({})), &[]),
        &ro,
        Box::new(move |s, a, c| {
            let a: OrgOnly = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            listing(s.list(&org)?, bindings(&org))
        }),
    )?;
    add(
        "secret_inspect",
        "Inspect a secret",
        "One secret's metadata (never its value).",
        obj(props(name_prop()), &["name"]),
        &ro,
        Box::new(|s, a, c| {
            let a: Named = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            Ok(serde_json::to_value(s.inspect(&org, &a.name)?)?)
        }),
    )?;
    let used = in_use;
    add(
        "secret_delete",
        "Delete a secret",
        "Delete a secret. Refused while a deployed stack's services use it.",
        obj(props(name_prop()), &["name"]),
        &destructive,
        Box::new(move |s, a, c| {
            let a: Named = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            s.inspect(&org, &a.name)?;
            let stacks = used(&org, &a.name);
            if !stacks.is_empty() {
                return Err(Error::invalid(format!(
                    "secret {} is in use by stack {}; remove it from the stack first",
                    a.name,
                    stacks.join(", ")
                )));
            }
            s.delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }),
    )?;
    add(
        "secret_refresh",
        "Refresh a secret",
        "Re-read an externally stored secret from its source now; if its version moved, each stack service using it acts per its `on_change` (`services`, `rolled`, `skipped` as for secret_set). `name` is a store name, or a stack's driver reference. A no-op for the local store.",
        obj(
            props(
                json!({"name": {"type": "string", "description": "A store name, or a driver reference a stack uses."}}),
            ),
            &["name"],
        ),
        &write,
        Box::new(move |s, a, c| {
            let a: Named = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            // A driver reference (`op://...`) is no store name; only the
            // stacks know it.
            let meta = if crate::secrets::validate_name(&a.name).is_ok() {
                match s.refresh(&org, &a.name) {
                    Ok(m) => Some(m),
                    Err(Error::NotFound(_)) => None,
                    Err(e) => return Err(e),
                }
            } else {
                None
            };
            let (refs, outcome) = refresh(&org, &a.name)?;
            let mut v = match (meta, refs.first()) {
                (Some(m), _) => serde_json::to_value(m)?,
                (None, Some((driver, version))) => {
                    json!({"org": org, "name": a.name, "driver": driver, "version": version})
                }
                (None, None) => return Err(crate::secrets::not_found(&org, &a.name)),
            };
            outcome.fill(&mut v);
            Ok(v)
        }),
    )?;
    add(
        "secret_resolve",
        "Resolve compose secrets",
        "Local callers only (`isb up`): the values (base64) of a compose file's store-backed secrets (`external`, `age`, `driver`), read from the org's store and decrypted with the daemon's key.",
        obj(
            props(
                json!({"secrets": {"type": "object", "description": "Top-level compose secrets, by key."}}),
            ),
            &["secrets"],
        ),
        &ro,
        Box::new(|s, a, c| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                org: Option<String>,
                secrets: BTreeMap<String, crate::spec::SecretDef>,
            }
            // A decryption oracle for the daemon's key: superadmins only
            // (the socket's `isb up`, or an HTTP caller with its reach),
            // whatever --deny-tools says.
            if !c.is_trusted() {
                return Err(Error::invalid(
                    "secret_resolve is for superadmins (the local socket: isb up) only",
                ));
            }
            let a: A = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            let values = crate::stack::secrets::resolve(s, &org, &a.secrets)?;
            let out: BTreeMap<String, String> = values
                .iter()
                .map(|(k, v)| (k.clone(), crate::rpc::b64_encode(v)))
                .collect();
            Ok(json!({"values": out}))
        }),
    )?;
    add(
        "secret_reencrypt",
        "Re-encrypt secrets",
        "Re-encrypt every stored value in the org (or every org, with all=true) to the current recipients: the daemon's key and the break-glass recipients. Run after changing the recipients.",
        obj(
            props(json!({"all": {"type": "boolean", "description": "Every org."}})),
            &[],
        ),
        &write,
        Box::new(|s, a, c| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                org: Option<String>,
                #[serde(default)]
                all: bool,
            }
            let a: A = args(a)?;
            if a.all && a.org.is_some() {
                return Err(Error::invalid("pass org or all, not both"));
            }
            // P1.7: `all` spans orgs; only platform admins may use it.
            let n = if a.all {
                s.reencrypt(None)?
            } else {
                s.reencrypt(Some(&org_for(c, a.org.as_deref())?))?
            };
            Ok(json!({"reencrypted": n, "recipients": s.recipients()}))
        }),
    )?;
    add(
        "secret_recipients",
        "Secret recipients",
        "The age recipients values are encrypted to (public keys only): the daemon's own key first, then the break-glass recipients. `isb secret encrypt` encrypts compose `age:` values to these.",
        obj(props(json!({})), &[]),
        &ro,
        Box::new(|s, a, c| {
            let a: OrgOnly = args(a)?;
            org_for(c, a.org.as_deref())?;
            Ok(json!({
                "public_key": s.keyring().public_key(),
                "recipients": s.recipients(),
            }))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{Keyring, LocalDriver};

    fn registry(dir: &std::path::Path) -> Registry {
        let k = Keyring::new(age::x25519::Identity::generate(), vec![]);
        let s = Arc::new(Secrets::new(LocalDriver::new(dir, Arc::new(k))));
        let in_use: InUse = Arc::new(|org: &OrgId, name: &str| {
            if org.is_default() && name == "used" {
                vec!["app".to_string()]
            } else {
                vec![]
            }
        });
        let cycle = |stack: &str, service: &str, action| Cycle {
            stack: stack.into(),
            service: service.into(),
            key: "k".into(),
            secret: "used".into(),
            from: 1,
            to: 2,
            action,
            error: None,
        };
        let changed: Changed = Arc::new(move |org: &OrgId, name: &str| {
            if org.is_default() && name == "used" {
                Outcome {
                    cycles: vec![
                        cycle("app", "web", crate::spec::OnChange::Restart),
                        cycle("app", "api", crate::spec::OnChange::Roll),
                        cycle("other", "cron", crate::spec::OnChange::None),
                    ],
                    skipped: vec![Skipped {
                        kind: "workspace".into(),
                        name: "main".into(),
                        reason: "delivered".into(),
                    }],
                }
            } else {
                Outcome::default()
            }
        });
        let refresh: Refresh = Arc::new(move |_org: &OrgId, name: &str| {
            if name == "op://v/item" {
                Ok((
                    vec![("vault".to_string(), 4)],
                    Outcome {
                        cycles: vec![cycle("app", "web", crate::spec::OnChange::Roll)],
                        skipped: vec![],
                    },
                ))
            } else {
                Ok((vec![], Outcome::default()))
            }
        });
        let bindings: Bindings = Arc::new(|org: &OrgId| {
            let b = |name: &str, driver: &str, stack: &str| Binding {
                name: name.into(),
                driver: driver.into(),
                version: 3,
                stack: stack.into(),
            };
            if org.is_default() {
                vec![
                    b("used", "local", "web"),
                    b("used", "local", "app"),
                    b("used", "local", "app"),
                    b("vault/item/field", "onepassword", "app"),
                ]
            } else {
                vec![]
            }
        });
        let mut r = Registry::new();
        register(
            &mut r,
            s,
            Hooks {
                prepare: Arc::new(|_, name: &str, v: &[u8]| {
                    if name == "locked" && v == b"bad" {
                        return Err(Error::invalid("rotate failed"));
                    }
                    Ok(vec![])
                }),
                in_use,
                changed,
                refresh,
                bindings,
            },
        )
        .unwrap();
        r
    }

    #[test]
    fn set_and_refresh_roll_and_resolve_is_local_only() {
        let dir = tempfile::tempdir().unwrap();
        let r = registry(dir.path());
        let v = crate::rpc::b64_encode(b"one");
        call(&r, "secret_create", json!({"name": "used", "value": v})).unwrap();
        let m = call(&r, "secret_set", json!({"name": "used", "value": v})).unwrap();
        // Only stacks that roll or restart count as rolled; a `none`
        // service is listed, with its action, but not cycled.
        assert_eq!(m["rolled"], json!(["app"]));
        assert_eq!(m["version"], 2);
        let actions: Vec<(String, String)> = m["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    format!(
                        "{}/{}",
                        c["stack"].as_str().unwrap(),
                        c["service"].as_str().unwrap()
                    ),
                    c["action"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                ("app/web".to_string(), "restart".to_string()),
                ("app/api".to_string(), "roll".to_string()),
                ("other/cron".to_string(), "none".to_string()),
            ]
        );
        assert_eq!(m["services"][0]["from"], 1);
        assert_eq!(m["services"][0]["to"], 2);
        assert_eq!(m["skipped"][0]["kind"], "workspace");
        // A driver reference is no store name; the stacks answer for it.
        let m = call(&r, "secret_refresh", json!({"name": "op://v/item"})).unwrap();
        assert_eq!(
            (
                m["driver"].as_str(),
                m["version"].as_u64(),
                m["rolled"].clone()
            ),
            (Some("vault"), Some(4), json!(["app"]))
        );
        assert!(matches!(
            call(&r, "secret_refresh", json!({"name": "op://v/other"})),
            Err(Error::NotFound(_))
        ));
        let m = call(&r, "secret_refresh", json!({"name": "used"})).unwrap();
        assert_eq!(m["version"], 2);
        // secret_resolve reads store-backed sources for isb up...
        let res = call(
            &r,
            "secret_resolve",
            json!({"secrets": {"a": {"external": true, "name": "used"}, "f": {"file": "./x"}}}),
        )
        .unwrap();
        assert_eq!(res["values"]["a"], json!(v));
        assert!(res["values"].get("f").is_none());
        // ...and never for a remote caller.
        let remote = Caller::Unauthenticated {
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        let e = (r.get("secret_resolve").unwrap().handler)(
            json!({"secrets": {"a": {"external": true, "name": "used"}}}),
            &remote,
        )
        .unwrap_err();
        assert!(e.to_string().contains("superadmins"), "{e}");
    }

    #[test]
    fn list_says_who_uses_what() {
        let dir = tempfile::tempdir().unwrap();
        let r = registry(dir.path());
        let v = crate::rpc::b64_encode(b"x");
        for n in ["used", "idle"] {
            call(&r, "secret_create", json!({"name": n, "value": v})).unwrap();
        }
        let l = call(&r, "secret_list", json!({})).unwrap();
        let by = |n: &str| {
            l["secrets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == n)
                .unwrap()["used_by"]
                .clone()
        };
        assert_eq!(by("used"), json!(["app", "web"]));
        assert_eq!(by("idle"), json!([]));
        assert_eq!(
            l["references"],
            json!([{"name": "vault/item/field", "driver": "onepassword", "version": 3, "used_by": ["app"]}])
        );
        let l = call(&r, "secret_list", json!({"org": "norm"})).unwrap();
        assert_eq!(l["references"], json!([]));
    }

    fn call(r: &Registry, tool: &str, a: Value) -> Result<Value> {
        let local = Caller::Local { uid: None };
        (r.get(tool).unwrap().handler)(a, &local)
    }

    #[test]
    fn tools_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let r = registry(dir.path());
        let v = crate::rpc::b64_encode(b"pa\x00ss");
        let m = call(
            &r,
            "secret_create",
            json!({"name": "db", "value": v, "labels": {"a": "b"}}),
        )
        .unwrap();
        assert_eq!(
            (m["version"].as_u64(), m["org"].as_str()),
            (Some(1), Some("default"))
        );
        assert!(call(&r, "secret_create", json!({"name": "db", "value": v})).is_err());
        let g = call(&r, "secret_get", json!({"name": "db"})).unwrap();
        assert_eq!(
            crate::rpc::b64_decode(g["value"].as_str().unwrap()).unwrap(),
            b"pa\x00ss"
        );
        let m = call(
            &r,
            "secret_set",
            json!({"name": "db", "value": crate::rpc::b64_encode(b"new")}),
        )
        .unwrap();
        assert_eq!(m["version"], 2);
        assert_eq!(m["labels"]["a"], "b");
        // Orgs are separate.
        call(
            &r,
            "secret_create",
            json!({"org": "norm", "name": "db", "value": v}),
        )
        .unwrap();
        let l = call(&r, "secret_list", json!({"org": "norm"})).unwrap();
        assert_eq!(l["secrets"].as_array().unwrap().len(), 1);
        assert!(call(&r, "secret_list", json!({"org": "Bad Org"})).is_err());
        // Listing and inspecting never carry the value.
        let l = call(&r, "secret_list", json!({})).unwrap();
        let i = call(&r, "secret_inspect", json!({"name": "db"})).unwrap();
        for v in [&l, &i] {
            let t = v.to_string();
            assert!(!t.contains("value") && !t.contains("bmV3"), "{t}");
        }
        assert_eq!(i["version"], 2);
        assert_eq!(
            call(&r, "secret_refresh", json!({"name": "db"})).unwrap()["version"],
            2
        );
        let re = call(&r, "secret_reencrypt", json!({"all": true})).unwrap();
        assert_eq!(re["reencrypted"], 2);
        let re = call(&r, "secret_reencrypt", json!({"org": "norm"})).unwrap();
        assert_eq!(re["reencrypted"], 1);
        assert!(call(&r, "secret_reencrypt", json!({"org": "norm", "all": true})).is_err());
        let rc = call(&r, "secret_recipients", json!({})).unwrap();
        assert_eq!(rc["recipients"][0], rc["public_key"]);
        // Delete: refused while a stack uses it, NotFound when absent.
        call(&r, "secret_create", json!({"name": "used", "value": v})).unwrap();
        let e = call(&r, "secret_delete", json!({"name": "used"})).unwrap_err();
        assert!(e.to_string().contains("in use by stack app"), "{e}");
        call(&r, "secret_delete", json!({"name": "db"})).unwrap();
        assert!(matches!(
            call(&r, "secret_delete", json!({"name": "db"})),
            Err(Error::NotFound(_))
        ));
        // A failed rotate stores nothing.
        call(&r, "secret_create", json!({"name": "locked", "value": v})).unwrap();
        let bad = crate::rpc::b64_encode(b"bad");
        assert!(call(&r, "secret_set", json!({"name": "locked", "value": bad})).is_err());
        assert_eq!(
            call(&r, "secret_inspect", json!({"name": "locked"})).unwrap()["version"],
            1
        );
        // Bad input.
        assert!(call(&r, "secret_set", json!({"name": "x", "value": "!!"})).is_err());
        assert!(call(&r, "secret_get", json!({"name": "x", "extra": 1})).is_err());
        assert!(call(&r, "secret_get", json!({"name": "../etc"})).is_err());
    }
}
