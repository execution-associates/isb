//! A stack's instances: the spec one is created from, which exist in incus
//! (by their labels), and an instance's init pid and address.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::Value;

use super::super::secrets::LABEL_SECRETS;
use super::super::{LABEL_REV, LABEL_SERVICE, LABEL_SLOT, LABEL_STACK, StackDef};
use crate::client::{Client, encode_query, encode_segment};
use crate::error::Result;
use crate::spec::{RestartMode, SandboxSpec};

/// A stack's instances (of one service), using incus' server-side filter.
#[doc(hidden)]
pub fn list_instances(client: &Client, stack: &str, service: Option<&str>) -> Result<Vec<Inst>> {
    let mut filter = format!("config.user.{LABEL_STACK} eq {stack}");
    if let Some(s) = service {
        filter.push_str(&format!(" and config.user.{LABEL_SERVICE} eq {s}"));
    }
    let v = client.get(&format!(
        "/1.0/instances?recursion=1&filter={}",
        encode_query(&filter)
    ))?;
    let mut out = Vec::new();
    for i in v.as_array().into_iter().flatten() {
        let info = crate::sandbox::SandboxInfo::from_api(i);
        let c = &info.config;
        // Filter again: an incus without filter support returns everything.
        if c.get(&format!("user.{LABEL_STACK}")).map(String::as_str) != Some(stack) {
            continue;
        }
        let svc = c
            .get(&format!("user.{LABEL_SERVICE}"))
            .cloned()
            .unwrap_or_default();
        if service.is_some_and(|s| s != svc) {
            continue;
        }
        out.push(Inst {
            name: info.name.clone(),
            slot: c
                .get(&format!("user.{LABEL_SLOT}"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            rev: c
                .get(&format!("user.{LABEL_REV}"))
                .cloned()
                .unwrap_or_default(),
            status: info.status.clone(),
            secrets: super::super::secrets::parse_versions_label(
                c.get(&format!("user.{LABEL_SECRETS}")).map(String::as_str),
            ),
            boot: c
                .iter()
                .filter(|(k, _)| k.starts_with("boot.autostart"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        });
    }
    out.sort_by(|a, b| (a.slot, &a.name).cmp(&(b.slot, &b.name)));
    Ok(out)
}

/// An instance's init pid (changes on every start) and its first global
/// address on any interface but loopback, IPv4 preferred.
pub(super) fn instance_state(client: &Client, name: &str) -> Result<(i64, Option<IpAddr>)> {
    let v = client.get(&format!("/1.0/instances/{}/state", encode_segment(name)))?;
    let pid = v.get("pid").and_then(Value::as_i64).unwrap_or(0);
    let mut v4 = None;
    let mut v6 = None;
    if let Some(nets) = v.get("network").and_then(Value::as_object) {
        for (ifname, n) in nets {
            if ifname == "lo" {
                continue;
            }
            for a in n
                .get("addresses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if a.get("scope").and_then(Value::as_str) != Some("global") {
                    continue;
                }
                let Some(ip) = a
                    .get("address")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<IpAddr>().ok())
                else {
                    continue;
                };
                match ip {
                    IpAddr::V4(_) if v4.is_none() => v4 = Some(ip),
                    IpAddr::V6(_) if v6.is_none() => v6 = Some(ip),
                    _ => {}
                }
            }
        }
    }
    Ok((pid, v4.or(v6)))
}

/// The spec an instance of `service` is created from: labelled, with its
/// published TCP ports removed (the balancer serves them) and its UDP ports
/// as NAT proxies (see [`super::ports`]), and always long-running, as swarm
/// ignores `restart` in favour of `restart_policy`.
pub(super) fn instance_spec(
    def: &StackDef,
    service: &str,
    spec: &SandboxSpec,
    slot: u32,
    rev: &str,
) -> Result<SandboxSpec> {
    let mut s = spec.clone();
    s.image = def.instance_image(service, &spec.image);
    s.restart = Some(RestartMode::Always);
    let boot = s.boot.get_or_insert_with(Default::default);
    if boot.priority.is_none() {
        boot.priority = Some(super::boot::boot_priority(slot));
    }
    (s.ports, s.stack_udp) = super::super::ports::instance_ports(spec)?;
    s.domains.clear();
    if let Some(d) = &s.deploy {
        s.labels.extend(d.labels.clone());
    }
    s.labels.insert(LABEL_STACK.into(), def.name.clone());
    s.labels.insert(LABEL_SERVICE.into(), service.into());
    s.labels.insert(LABEL_SLOT.into(), slot.to_string());
    s.labels.insert(LABEL_REV.into(), rev.into());
    let live = live_versions(def, service);
    if !live.is_empty() {
        s.labels.insert(
            LABEL_SECRETS.into(),
            super::super::secrets::versions_label(&live),
        );
    }
    Ok(s)
}

/// The versions bound now of the secrets `service` takes in place.
pub(super) fn live_versions(def: &StackDef, service: &str) -> BTreeMap<String, u64> {
    def.live_secrets(service)
        .into_iter()
        .map(|(k, (_, v))| (k, v))
        .collect()
}

/// A stack's instance as listed.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct Inst {
    pub name: String,
    pub(in crate::stack) slot: u32,
    pub rev: String,
    pub(super) status: String,
    /// [`LABEL_SECRETS`]: the `restart`/`none` secret versions its app last
    /// started with; `None` on an instance from before the label.
    pub(super) secrets: Option<BTreeMap<String, u64>>,
    /// Its `boot.autostart*` config.
    pub(super) boot: crate::plan::Props,
}

impl Inst {
    #[doc(hidden)]
    pub fn is_running(&self) -> bool {
        self.running()
    }

    pub(super) fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
}
