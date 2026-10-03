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

type Handler = Box<dyn Fn(&Secrets, Value, &Caller) -> Result<Value> + Send + Sync>;

/// The stacks in an org whose services use a stored secret.
pub type InUse = Arc<dyn Fn(&OrgId, &str) -> Vec<String> + Send + Sync>;

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

/// Register the secret tools.
pub fn register(r: &mut Registry, secrets: Arc<Secrets>, in_use: InUse) -> Result<()> {
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
    add(
        "secret_set",
        "Set a secret",
        "Give a secret a new value (base64), bumping its version; creates it in the local store if missing. Stacks using it roll to the new version.",
        obj(
            props(json!({"name": name_prop()["name"], "value": value_prop["value"]})),
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
            }
            let a: A = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            Ok(serde_json::to_value(s.set(
                &org,
                &a.name,
                &value_of(&a.value)?,
            )?)?)
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
        "An org's secrets: name, driver, version, timestamps, labels. Never values.",
        obj(props(json!({})), &[]),
        &ro,
        Box::new(|s, a, c| {
            let a: OrgOnly = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            Ok(json!({"secrets": s.list(&org)?}))
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
        "Delete a secret. Refused while a deployed stack's services use it (`external: true`).",
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
        "Re-read an externally stored secret from its source now. A no-op for the local store.",
        obj(props(name_prop()), &["name"]),
        &write,
        Box::new(|s, a, c| {
            let a: Named = args(a)?;
            let org = org_for(c, a.org.as_deref())?;
            Ok(serde_json::to_value(s.refresh(&org, &a.name)?)?)
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
        let mut r = Registry::new();
        register(&mut r, s, in_use).unwrap();
        r
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
        // Bad input.
        assert!(call(&r, "secret_set", json!({"name": "x", "value": "!!"})).is_err());
        assert!(call(&r, "secret_get", json!({"name": "x", "extra": 1})).is_err());
        assert!(call(&r, "secret_get", json!({"name": "../etc"})).is_err());
    }
}
