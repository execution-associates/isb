//! Reading a stack's instances from incus: which exist (by their labels)
//! and an instance's init pid and address.

use std::net::IpAddr;

use serde_json::Value;

use super::super::secrets::LABEL_SECRETS;
use super::super::{LABEL_REV, LABEL_SERVICE, LABEL_SLOT, LABEL_STACK};
use super::Inst;
use crate::client::{Client, encode_query, encode_segment};
use crate::error::Result;

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
