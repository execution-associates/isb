//! The `org_*` tools: orgs over MCP and REST, for the web UI's settings and
//! admin pages. Reading an org is for its members; creating, changing and
//! deleting one is for platform admins (`PLATFORM_TOOLS`), since limits and
//! egress exceptions are what keep one org from another and from the host.
//! The domain allowlist and ingress provider are a platform admin's too: they
//! decide which public names an org may claim. Bind roots are host paths and
//! stay with the CLI (`isb org create --bind-root`).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args};
use crate::error::{Error, Result};
use crate::org::{self, Egress, OrgId, OrgInfo, OrgOptions};
use crate::server::{Caller, Registry, Tool};

/// An org-wide limit as the tools take it: a value, or `"none"` (or
/// `null`) to lift it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LimitArg<T> {
    Set(T),
    Lift,
}

/// A present limit field: `null` or `"none"` lifts it, anything else is its
/// value. An absent one stays `None` (`#[serde(default)]`): kept.
fn limit_arg<'de, D, T>(d: D) -> std::result::Result<Option<LimitArg<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    match Value::deserialize(d)? {
        Value::Null => Ok(Some(LimitArg::Lift)),
        Value::String(s) if s.trim().eq_ignore_ascii_case("none") => Ok(Some(LimitArg::Lift)),
        v => serde_json::from_value(v)
            .map(|t| Some(LimitArg::Set(t)))
            .map_err(serde::de::Error::custom),
    }
}

/// Limits and egress as the tools take them. `None` keeps what the org has.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default, deserialize_with = "limit_arg")]
    pub cpus: Option<LimitArg<u32>>,
    #[serde(default, deserialize_with = "limit_arg")]
    pub memory: Option<LimitArg<String>>,
    #[serde(default, deserialize_with = "limit_arg")]
    pub disk: Option<LimitArg<String>>,
    #[serde(default, deserialize_with = "limit_arg")]
    pub instances: Option<LimitArg<u32>>,
    #[serde(default)]
    pub default_cpus: Option<u32>,
    #[serde(default)]
    pub default_memory: Option<String>,
    /// Replaces the exceptions; `[]` clears them.
    #[serde(default)]
    pub egress: Option<Vec<String>>,
    /// UDP ports (`IP:PORT`) the org's stacks may publish. Replaces the
    /// list; `[]` clears it.
    #[serde(default)]
    pub udp: Option<Vec<String>>,
    /// Domain suffixes the org's services may serve. Replaces the list;
    /// `[]` clears it (any concrete name).
    #[serde(default)]
    pub domains: Option<Vec<String>>,
    /// `caddy` or `cloudflare-tunnel`.
    #[serde(default)]
    pub ingress: Option<String>,
    /// The tunnel provider's Cloudflare ids; `""` clears one.
    #[serde(default)]
    pub cloudflare_account: Option<String>,
    #[serde(default)]
    pub cloudflare_zone: Option<String>,
    /// Where the org runs: `local` (this daemon) or a server's name. A
    /// control plane routes a server placement before the tool runs.
    #[serde(default)]
    pub server: Option<String>,
    /// Where the org runs: `"local"`, `{"server": NAME}` or `{"vm": {cpus,
    /// memory, disk}}` (a dedicated VM). A control plane routes anything
    /// but local before the tool runs.
    #[serde(default)]
    pub placement: Option<Value>,
    /// With a dedicated VM: wait for it (default), or answer at once with
    /// the provisioning to follow. Read by the control plane's router.
    #[serde(default)]
    #[allow(dead_code)]
    pub wait: Option<bool>,
}

impl Settings {
    fn org(&self) -> Result<OrgId> {
        let mut where_ = json!({});
        if let Some(s) = &self.server {
            where_["server"] = json!(s);
        }
        if let Some(p) = &self.placement {
            where_["placement"] = p.clone();
        }
        match crate::servers::vm::placement(&where_)? {
            crate::servers::vm::Placement::Local => {}
            crate::servers::vm::Placement::Server(s) => {
                return Err(Error::invalid(format!(
                    "server {s}: this daemon places no orgs on servers (only a control plane does: docs/guides/servers.md)"
                )));
            }
            crate::servers::vm::Placement::Vm(_) => {
                return Err(Error::invalid(
                    "a dedicated VM is made by a control plane; this daemon is a server's agent (docs/guides/servers.md)",
                ));
            }
        }
        let o = OrgId::new(
            self.org
                .clone()
                .ok_or_else(|| Error::invalid("org is required"))?,
        )?;
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
        let udp = list(self.udp.as_deref(), org::check_udp_port)?;
        let domains = list(self.domains.as_deref(), org::check_domain_suffix)?;
        let parse_u32 = |v: Option<&String>| v.and_then(|s| s.parse::<u32>().ok());
        let set = |v: &Option<LimitArg<String>>| match v {
            Some(LimitArg::Set(s)) => Some(s.clone()),
            _ => None,
        };
        let count = |v: &Option<LimitArg<u32>>| match v {
            Some(LimitArg::Set(n)) => Some(*n),
            _ => None,
        };
        let (memory, disk) = (set(&self.memory), set(&self.disk));
        for (what, v) in [
            ("memory", &memory),
            ("disk", &disk),
            ("default_memory", &self.default_memory),
        ] {
            if let Some(v) = v {
                check_size(what, v)?;
            }
        }
        let (cpus, instances) = (count(&self.cpus), count(&self.instances));
        for (what, v) in [
            ("cpus", cpus),
            ("instances", instances),
            ("default_cpus", self.default_cpus),
        ] {
            if v == Some(0) {
                return Err(Error::invalid(format!(
                    "{what} must be at least 1 (\"none\" lifts the limit)"
                )));
            }
        }
        let lift = [
            (org::Limit::Cpus, matches!(self.cpus, Some(LimitArg::Lift))),
            (
                org::Limit::Memory,
                matches!(self.memory, Some(LimitArg::Lift)),
            ),
            (org::Limit::Disk, matches!(self.disk, Some(LimitArg::Lift))),
            (
                org::Limit::Instances,
                matches!(self.instances, Some(LimitArg::Lift)),
            ),
        ]
        .into_iter()
        .filter_map(|(l, lifted)| lifted.then_some(l))
        .collect();
        Ok(OrgOptions {
            cpus,
            memory,
            disk,
            instances,
            lift,
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
            udp,
            domains,
            ingress: self.ingress.as_ref().map(|s| s.trim().to_string()),
            cloudflare_account: self
                .cloudflare_account
                .as_ref()
                .map(|s| s.trim().to_string()),
            cloudflare_zone: self.cloudflare_zone.as_ref().map(|s| s.trim().to_string()),
        })
    }
}

/// A list argument, each entry trimmed and checked; blanks dropped. `None`
/// keeps what the org has.
fn list<T>(v: Option<&[String]>, check: impl Fn(&str) -> Result<T>) -> Result<Option<Vec<T>>> {
    v.map(|l| {
        l.iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(&check)
            .collect()
    })
    .transpose()
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
/// The orgs that exist: this host's (incus projects with isb's marker)
/// and, on a control plane, the ones it placed on its servers. What
/// `whoami` lists, rather than the identity store's org rows, which an org
/// made or removed past this daemon leaves behind.
pub(super) fn existing(
    client: &crate::client::Client,
    servers: Option<&crate::servers::Servers>,
) -> Result<Vec<OrgId>> {
    let mut v = org::names(client)?;
    if let Some(s) = servers {
        v.extend(s.placements().into_keys());
    }
    v.sort();
    v.dedup();
    Ok(v)
}

/// [`existing`] for the identity endpoints and `whoami`.
pub(super) fn existing_fn(
    client: crate::client::Client,
    servers: Option<Arc<crate::servers::Servers>>,
) -> crate::auth::ops::OrgsFn {
    Arc::new(move || existing(&client, servers.as_deref()).map_err(|e| e.to_string()))
}

fn view(d: &Daemon, o: &OrgInfo) -> Value {
    let mut v = serde_json::to_value(o).unwrap_or_default();
    v["domain"] = json!(format!("{}.isb", o.name));
    v["service_names"] = json!(o.dns_dir.is_some());
    v["placement"] = super::servers::placement_view(d.servers.as_ref(), &o.name);
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
        "cpus": {"anyOf": [{"type": "integer", "minimum": 1}, {"type": "string", "enum": ["none"]}, {"type": "null"}], "description": "CPUs across the org: the sum of every instance's limits.cpu, stopped ones included. \"none\" or null lifts the limit."},
        "memory": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Memory across the org, e.g. 16GiB: the sum of every instance's limits.memory, stopped ones included. \"none\" or null lifts the limit."},
        "disk": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Disk across the org, e.g. 100GiB: the sum of every root disk's and volume's size. While set, each instance isb creates gets a root size of its own (raw_devices.root.size, else 10GiB); setting it is refused while an instance has none, naming each. \"none\" or null lifts the limit."},
        "instances": {"anyOf": [{"type": "integer", "minimum": 1}, {"type": "string", "enum": ["none"]}, {"type": "null"}], "description": "Instances in the org, stopped ones included. \"none\" or null lifts the limit."},
        "default_cpus": {"type": "integer", "minimum": 1, "description": "CPUs an instance gets when its spec sets none."},
        "default_memory": {"type": "string", "description": "Memory an instance gets when its spec sets none, e.g. 512MiB."},
        "egress": {"type": "array", "items": {"type": "string"}, "description": "Private destinations the org may reach, CIDR[:PORTS[/tcp|udp]] (docs/concepts/orgs.md). Replaces the list; [] clears it."},
        "udp": {"type": "array", "items": {"type": "string"}, "description": "UDP ports the org's stacks may publish on the host, IP:PORT each (a specific host address, e.g. 203.0.113.7:10000), forwarded by incus to the service's one replica with the client's address kept (docs/concepts/stacks.md). Replaces the list; [] clears it."},
        "domains": {"type": "array", "items": {"type": "string"}, "description": "Domain suffixes the org's services may serve: example.com allows it and every name under it, *.example.com wildcard hosts too. Replaces the list; [] clears it (any concrete name, no wildcards). The same as `isb org create --allow-domain`."},
        "ingress": {"type": "string", "enum": ["caddy", "cloudflare-tunnel"], "description": "How the org's domains are reached: caddy (the server's public listeners) or cloudflare-tunnel (the org's own tunnel, token in its secret cloudflare-tunnel-token)."},
        "cloudflare_account": {"type": "string", "description": "Cloudflare account id for the tunnel's API calls (default: the tunnel token's); \"\" clears it."},
        "cloudflare_zone": {"type": "string", "description": "Cloudflare zone id the org's hostnames are in (default: looked up per hostname); \"\" clears it."},
        "server": {"type": "string", "description": "Where the org runs: local (default) or a server's name (server_list). Set at creation; an org is not moved between servers. Same as placement {\"server\": NAME}."},
        "placement": {
            "description": "Where the org runs, set at creation: \"local\" (this host: an incus project sharing its kernel), {\"server\": NAME} (another host, server_list), or {\"vm\": {\"cpus\", \"memory\", \"disk\"}} (a dedicated VM this control plane makes on its own host: the org's own kernel; defaults 2 CPUs, 4GiB, 40GiB). An org is not moved afterwards.",
            "oneOf": [
                {"type": "string", "enum": ["local"]},
                {"type": "object", "properties": {"server": {"type": "string"}}, "required": ["server"], "additionalProperties": false},
                {"type": "object", "properties": {"vm": {"type": "object", "properties": {
                    "cpus": {"type": "integer", "minimum": 1, "maximum": 256},
                    "memory": {"type": "string", "description": "At least 2GiB (default 4GiB)."},
                    "disk": {"type": "string", "description": "At least 10GiB (default 40GiB)."}
                }, "additionalProperties": false}}, "required": ["vm"], "additionalProperties": false}
            ]
        },
        "wait": {"type": "boolean", "description": "With a dedicated VM: wait until it is made and the org created (default true; minutes). false answers at once with `provision`; follow it with server_provision_get (name vm-<org>)."}
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
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
        "An org's limits with what is allocated against each (`allocation`: limit, allocated, free; allocated is the sum of every instance's limit, stopped ones included, which is what incus enforces), per-instance defaults, network (bridge and subnet), egress exceptions, bind roots and service-name domain, with its instance, stack and member counts.",
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
        "Platform admins: create an org (an incus project with its own bridge and network ACL), with optional limits, egress exceptions, domain allowlist and ingress provider. Fails if it exists. Bind roots are set from the host's CLI only.",
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
            // Services that were failing on a limit retry now.
            let woken = d.ctl.org_limits_changed(&id);
            if woken > 0 {
                notes.push(format!("{woken} service(s) waiting on a limit retry now"));
            }
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
        "Platform admins: change an org's limits, per-instance defaults, egress exceptions, the UDP ports its stacks may publish, its domain allowlist or its ingress provider. Fields left out keep their value; a limit given as \"none\" (or null) is lifted; `egress`, `udp` and `domains` replace their lists. The same as `isb org update` (and `isb org create`'s --allow-domain, --ingress and --cloudflare-* on an existing org).",
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
            // Services that were failing on a limit retry now.
            let woken = d.ctl.org_limits_changed(&id);
            if woken > 0 {
                notes.push(format!("{woken} service(s) waiting on a limit retry now"));
            }
            let mut v = view(d, &info);
            v["notes"] = json!(notes);
            Ok(v)
        }
    );
    tool!(
        "org_delete",
        "Delete an org",
        "Platform admins: delete an org: its project with its volumes, its network, ACL and service names, its members, invitations and tokens, and its metrics history. Refused while stacks are deployed in it (remove them first); with force=true its remaining sandboxes are deleted too. Its secrets stay on disk under the state directory.",
        schema(
            json!({
                "force": {"type": "boolean", "description": "Also delete the org's sandboxes."},
                "delete_vm": {"type": "boolean", "description": "For an org in a dedicated VM: delete the VM and its server registration too (default false: the VM keeps running as an empty server)."}
            }),
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
                // An org on this host has no VM of its own.
                #[serde(default)]
                #[allow(dead_code)]
                delete_vm: bool,
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
            // Its metrics history, and its state directory when that leaves
            // it empty (secrets stay).
            d.history.forget(&id);
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
            default_disk: None,
            allocation: Default::default(),
            bind_roots: vec!["/srv/acme".into()],
            egress: vec!["10.9.0.0/16".into()],
            domains: vec![],
            ingress: "caddy".into(),
            udp: vec![],
            cloudflare_account: None,
            cloudflare_zone: None,
            dns_dir: None,
            instances: 0,
            allow_nesting: false,
        }
    }

    #[test]
    fn update_keeps_what_it_is_not_given() {
        let s = Settings {
            org: Some("acme".into()),
            memory: Some(LimitArg::Set("8GiB".into())),
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
    fn a_limit_given_as_none_or_null_is_lifted() {
        let s: Settings = serde_json::from_value(json!({
            "org": "acme", "cpus": "none", "disk": null, "memory": "4GiB", "instances": 3
        }))
        .unwrap();
        let o = s.options(Some(&info())).unwrap();
        assert_eq!(o.lift, vec![org::Limit::Cpus, org::Limit::Disk]);
        assert_eq!((o.cpus, o.disk), (None, None));
        assert_eq!(o.memory.as_deref(), Some("4GiB"));
        assert_eq!(o.instances, Some(3));
        // Left out: kept, not lifted.
        let s: Settings = serde_json::from_value(json!({"org": "acme"})).unwrap();
        assert!(s.options(Some(&info())).unwrap().lift.is_empty());
        let bad: std::result::Result<Settings, _> = serde_json::from_value(json!({"cpus": "lots"}));
        assert!(bad.is_err());
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
    fn udp_ports_replace_keep_and_are_checked() {
        let s = Settings {
            udp: Some(vec!["203.0.113.7:10000".into(), " ".into()]),
            ..Default::default()
        };
        let u = s.options(None).unwrap().udp.unwrap();
        assert_eq!(u, vec!["203.0.113.7:10000".parse().unwrap()]);
        assert!(
            Settings::default()
                .options(Some(&info()))
                .unwrap()
                .udp
                .is_none()
        );
        for bad in ["0.0.0.0:10000", "10000", "127.0.0.1:53"] {
            let s = Settings {
                udp: Some(vec![bad.into()]),
                ..Default::default()
            };
            assert!(s.options(None).is_err(), "{bad}");
        }
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
            cpus: Some(LimitArg::Set(0)),
            ..Default::default()
        };
        assert!(s.options(None).is_err());
    }

    #[test]
    fn the_default_org_has_settings_like_any_other() {
        let s = Settings {
            org: Some("default".into()),
            ..Default::default()
        };
        assert!(s.org().unwrap().is_default());
        assert!(Settings::default().org().is_err());
    }

    #[test]
    fn only_local_placements_reach_the_tool() {
        let with = |p: Value| Settings {
            org: Some("acme".into()),
            placement: Some(p),
            ..Default::default()
        };
        assert!(with(json!("local")).org().is_ok());
        let e = with(json!({"vm": {}})).org().unwrap_err();
        assert!(e.to_string().contains("control plane"), "{e}");
        let e = with(json!({"server": "hel-1"})).org().unwrap_err();
        assert!(e.to_string().contains("server hel-1"), "{e}");
        let e = with(json!({"vm": {"memory": "1GiB"}})).org().unwrap_err();
        assert!(e.to_string().contains("at least 2 GiB"), "{e}");
        let both = Settings {
            org: Some("acme".into()),
            server: Some("x".into()),
            placement: Some(json!("local")),
            ..Default::default()
        };
        assert!(both.org().is_err());
    }
}
