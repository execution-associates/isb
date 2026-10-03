//! The `org_*` tools: orgs over MCP and REST, for the web UI's settings and
//! admin pages. Reading an org is for its members; creating, changing and
//! deleting one is for platform admins (`PLATFORM_TOOLS`), since limits and
//! egress exceptions are what keep one org from another and from the host.
//! Bind roots are host paths and stay with the CLI (`isb org create
//! --bind-root`).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args};
use crate::error::{Error, Result};
use crate::org::{self, Egress, OrgId, OrgInfo, OrgOptions};
use crate::server::{Caller, Registry, Tool};

/// Limits and egress as the tools take them. `None` keeps what the org has.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub cpus: Option<u32>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub disk: Option<String>,
    #[serde(default)]
    pub instances: Option<u32>,
    #[serde(default)]
    pub default_cpus: Option<u32>,
    #[serde(default)]
    pub default_memory: Option<String>,
    /// Replaces the exceptions; `[]` clears them.
    #[serde(default)]
    pub egress: Option<Vec<String>>,
}

impl Settings {
    fn org(&self) -> Result<OrgId> {
        let o = OrgId::new(
            self.org
                .clone()
                .ok_or_else(|| Error::invalid("org is required"))?,
        )?;
        if o.is_default() {
            return Err(Error::invalid(
                "the default org is incus' default project; it has no settings",
            ));
        }
        Ok(o)
    }

    /// The options for `org::ensure`: these settings over what the org has
    /// now (`current`), so a field left out is kept, bind roots included.
    pub(super) fn options(&self, current: Option<&OrgInfo>) -> Result<OrgOptions> {
        let egress = match &self.egress {
            Some(list) => {
                let e = list
                    .iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(Egress::parse)
                    .collect::<Result<Vec<_>>>()?;
                org::check_egress(&e)?;
                Some(e)
            }
            None => None,
        };
        let parse_u32 = |v: Option<&String>| v.and_then(|s| s.parse::<u32>().ok());
        for (what, v) in [
            ("memory", &self.memory),
            ("disk", &self.disk),
            ("default_memory", &self.default_memory),
        ] {
            if let Some(v) = v {
                check_size(what, v)?;
            }
        }
        for (what, v) in [
            ("cpus", self.cpus),
            ("instances", self.instances),
            ("default_cpus", self.default_cpus),
        ] {
            if v == Some(0) {
                return Err(Error::invalid(format!("{what} must be at least 1")));
            }
        }
        Ok(OrgOptions {
            cpus: self.cpus,
            memory: self.memory.clone(),
            disk: self.disk.clone(),
            instances: self.instances,
            default_cpus: self
                .default_cpus
                .or_else(|| parse_u32(current.and_then(|c| c.default_cpus.as_ref()))),
            default_memory: self
                .default_memory
                .clone()
                .or_else(|| current.and_then(|c| c.default_memory.clone())),
            bind_roots: current
                .map(|c| c.bind_roots.iter().map(Into::into).collect())
                .unwrap_or_default(),
            egress,
            // Domain allowlist and ingress provider stay as they are: they
            // are set with `isb org create`, not through this tool.
            ..Default::default()
        })
    }
}

/// A size incus takes: digits, then an optional unit (`512MiB`, `16GiB`,
/// `100GB`).
fn check_size(what: &str, v: &str) -> Result<()> {
    let v = v.trim();
    let digits = v.chars().take_while(char::is_ascii_digit).count();
    let unit = &v[digits..];
    const UNITS: &[&str] = &[
        "", "B", "kB", "MB", "GB", "TB", "PB", "EB", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB",
    ];
    if digits == 0 || !UNITS.contains(&unit) {
        return Err(Error::invalid(format!(
            "{what} {v:?}: a size such as 512MiB or 16GiB"
        )));
    }
    Ok(())
}

/// An org as the tools answer it: the incus side, the service-name domain,
/// and how many members it has.
fn view(d: &Daemon, o: &OrgInfo) -> Value {
    let mut v = serde_json::to_value(o).unwrap_or_default();
    v["domain"] = json!(format!("{}.isb", o.name));
    v["service_names"] = json!(o.dns_dir.is_some());
    v["members"] = json!(d.users.list_members(&o.name).map(|m| m.len()).unwrap_or(0));
    v["stacks"] = json!(
        d.ctl
            .definitions()
            .iter()
            .filter(|s| s.org == o.name)
            .count()
    );
    v
}

fn schema(extra: Value, required: &[&str], org_desc: &str) -> Value {
    let mut props = extra;
    props["org"] = json!({"type": "string", "description": org_desc});
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn settings_props() -> Value {
    json!({
        "cpus": {"type": "integer", "minimum": 1, "description": "CPUs across the org's instances."},
        "memory": {"type": "string", "description": "Memory across the org, e.g. 16GiB."},
        "disk": {"type": "string", "description": "Disk across the org, e.g. 100GiB."},
        "instances": {"type": "integer", "minimum": 1, "description": "Instances in the org."},
        "default_cpus": {"type": "integer", "minimum": 1, "description": "CPUs an instance gets when its spec sets none."},
        "default_memory": {"type": "string", "description": "Memory an instance gets when its spec sets none, e.g. 512MiB."},
        "egress": {"type": "array", "items": {"type": "string"}, "description": "Private destinations the org may reach, CIDR[:PORTS[/tcp|udp]] (docs/orgs.md). Replaces the list; [] clears it."}
    })
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let d = d.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| f(&d, a, c))
                    .title($title)
                    .annotations($ann.clone()),
            )?;
        }};
    }

    tool!(
        "org_get",
        "Show an org",
        "An org's limits, per-instance defaults, network (bridge and subnet), egress exceptions, bind roots and service-name domain, with its instance, stack and member counts.",
        schema(json!({}), &[], "The org (default: default)."),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let o = match a.org {
                Some(o) => OrgId::new(o)?,
                None => OrgId::default_org(),
            };
            Ok(view(d, &org::get(&d.client, &o)?))
        }
    );
    tool!(
        "org_list",
        "List orgs",
        "Platform admins: every org, as org_get shows one.",
        schema(json!({}), &[], "Ignored."),
        ro,
        |d: &Daemon, _a: Value, _c: &Caller| -> Result<Value> {
            let orgs: Vec<Value> = org::list(&d.client)?.iter().map(|o| view(d, o)).collect();
            Ok(json!({"orgs": orgs}))
        }
    );
    tool!(
        "org_create",
        "Create an org",
        "Platform admins: create an org (an incus project with its own bridge and network ACL), with optional limits and egress exceptions. Fails if it exists. Bind roots are set from the host's CLI only.",
        schema(
            settings_props(),
            &["org"],
            "The new org's name: [a-z0-9-], starts with a letter."
        ),
        write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let s: Settings = args(a)?;
            let id = s.org()?;
            match org::get(&d.client, &id) {
                Ok(_) => return Err(Error::AlreadyExists(format!("org {id}"))),
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
            let opts = s.options(None)?;
            let mut notes = Vec::new();
            let info = org::ensure(&d.client, &id, &opts, &mut |m: &str| {
                notes.push(m.to_string())
            })?;
            d.users
                .ensure_org(&info.name)
                .map_err(|e| Error::invalid(e.to_string()))?;
            let mut v = view(d, &info);
            v["notes"] = json!(notes);
            Ok(v)
        }
    );
    tool!(
        "org_update",
        "Change an org",
        "Platform admins: change an org's limits, per-instance defaults or egress exceptions. Fields left out keep their value; `egress` replaces the list. A limit cannot be lifted once set (as with `isb org create`).",
        schema(settings_props(), &["org"], "The org."),
        write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let s: Settings = args(a)?;
            let id = s.org()?;
            let current = org::get(&d.client, &id)?;
            let opts = s.options(Some(&current))?;
            let mut notes = Vec::new();
            let info = org::ensure(&d.client, &id, &opts, &mut |m: &str| {
                notes.push(m.to_string())
            })?;
            let mut v = view(d, &info);
            v["notes"] = json!(notes);
            Ok(v)
        }
    );
    tool!(
        "org_delete",
        "Delete an org",
        "Platform admins: delete an org: its project with its volumes, its network, ACL and service names, and its members, invitations and tokens. Refused while stacks are deployed in it (remove them first); with force=true its remaining sandboxes are deleted too. Its secrets stay on disk under the state directory.",
        schema(
            json!({"force": {"type": "boolean", "description": "Also delete the org's sandboxes."}}),
            &["org"],
            "The org to delete."
        ),
        destructive,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                org: String,
                #[serde(default)]
                force: bool,
            }
            let a: A = args(a)?;
            let id = OrgId::new(a.org)?;
            if id.is_default() {
                return Err(Error::invalid("the default org cannot be removed"));
            }
            // The controller would recreate a stack's instances in a project
            // that no longer exists.
            let stacks: Vec<String> = d
                .ctl
                .definitions()
                .iter()
                .filter(|s| s.org == id)
                .map(|s| s.name.clone())
                .collect();
            if !stacks.is_empty() {
                return Err(Error::invalid(format!(
                    "org {id} has stacks deployed ({}); remove them first",
                    stacks.join(", ")
                )));
            }
            let mut notes = Vec::new();
            org::remove(&d.client, &id, a.force, &mut |m: &str| {
                notes.push(m.to_string())
            })?;
            d.users
                .delete_org(&id)
                .map_err(|e| Error::invalid(e.to_string()))?;
            Ok(json!({"ok": true, "notes": notes}))
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> OrgInfo {
        OrgInfo {
            name: OrgId::new("acme").unwrap(),
            project: "isb-acme".into(),
            network: Some("isbbr00000000".into()),
            subnet: Some("10.1.2.1/24".into()),
            cpus: Some("4".into()),
            memory: None,
            disk: None,
            instances_limit: None,
            default_cpus: Some("2".into()),
            default_memory: Some("1GiB".into()),
            bind_roots: vec!["/srv/acme".into()],
            egress: vec!["10.9.0.0/16".into()],
            domains: vec![],
            ingress: "caddy".into(),
            cloudflare_account: None,
            cloudflare_zone: None,
            dns_dir: None,
            instances: 0,
        }
    }

    #[test]
    fn update_keeps_what_it_is_not_given() {
        let s = Settings {
            org: Some("acme".into()),
            memory: Some("8GiB".into()),
            ..Default::default()
        };
        let o = s.options(Some(&info())).unwrap();
        assert_eq!(o.memory.as_deref(), Some("8GiB"));
        assert_eq!(o.cpus, None, "None keeps the project's limit");
        assert_eq!(o.default_cpus, Some(2));
        assert_eq!(o.default_memory.as_deref(), Some("1GiB"));
        assert_eq!(o.bind_roots, vec![std::path::PathBuf::from("/srv/acme")]);
        assert!(o.egress.is_none(), "egress left out is kept");
    }

    #[test]
    fn egress_replaces_and_clears() {
        let s = Settings {
            org: Some("acme".into()),
            egress: Some(vec!["100.79.171.47:1080/tcp".into(), " ".into()]),
            ..Default::default()
        };
        let e = s.options(Some(&info())).unwrap().egress.unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].render(), "100.79.171.47/32:1080/tcp");
        let s = Settings {
            egress: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(s.options(None).unwrap().egress, Some(vec![]));
        let s = Settings {
            egress: Some(vec!["not-a-cidr".into()]),
            ..Default::default()
        };
        assert!(s.options(None).is_err());
    }

    #[test]
    fn sizes_and_counts_are_checked() {
        for ok in ["512MiB", "16GiB", "100GB", "1024"] {
            assert!(check_size("memory", ok).is_ok(), "{ok}");
        }
        for bad in ["", "GiB", "16 gigs", "-1GiB", "16gib"] {
            assert!(check_size("memory", bad).is_err(), "{bad}");
        }
        let s = Settings {
            cpus: Some(0),
            ..Default::default()
        };
        assert!(s.options(None).is_err());
    }

    #[test]
    fn the_default_org_has_no_settings() {
        let s = Settings {
            org: Some("default".into()),
            ..Default::default()
        };
        assert!(s.org().is_err());
        assert!(Settings::default().org().is_err());
    }
}
