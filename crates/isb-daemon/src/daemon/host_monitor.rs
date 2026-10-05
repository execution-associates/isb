//! `host_monitor`: the host's live numbers (CPU per core, memory and swap,
//! pools, disk I/O, interfaces), its last hour, and every instance's rates,
//! for this host or a remote server, next to a card per server. Remote
//! servers answer the agent route `POST /internal/v1/monitor` with the same
//! shape; one too old for it shows its heartbeat's numbers (`partial`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::server::{Registry, Tool};

/// Chart points per answer: enough for a wide chart, few enough to poll.
const MAX_POINTS: usize = 300;
/// A remote's monitor answer must come back within the UI's poll or two.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(4);

pub const DEFAULT_RANGE: u64 = 300;

/// Seconds of history asked for, clamped to what the sampler keeps.
pub fn range(a: &Value) -> u64 {
    a.get("range")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_RANGE)
        .clamp(60, 3600)
}

/// This host's monitor answer.
pub fn local(ctl: &crate::stack::Controller, range: u64) -> Value {
    let snap = ctl.snapshot();
    let now = snap.at / 1000;
    let (step, points) = crate::metrics::downsample(&snap.host_history, range, now, MAX_POINTS);
    let instances: Vec<Value> = snap
        .instances
        .values()
        .map(|i| {
            json!({
                "name": i.name,
                "project": i.project,
                "org": crate::org::OrgId::from_incus_project(&i.project).map(|o| o.to_string()),
                "kind": i.kind,
                "status": i.status,
                "ip": i.ip,
                "stack": i.stack(),
                "cpu_pct": i.cpu_pct,
                "cpu_history": i.cpu_history,
                "mem_bytes": i.mem_bytes,
                "net_rx_rate": i.net_rx_rate,
                "net_tx_rate": i.net_tx_rate,
                "disk_read_rate": i.disk_read_rate,
                "disk_write_rate": i.disk_write_rate,
            })
        })
        .collect();
    let mut host = serde_json::to_value(&snap.host).unwrap_or_default();
    host["isb"] = json!(env!("CARGO_PKG_VERSION"));
    json!({
        "at": snap.at,
        "host": host,
        "history": {"step": step, "points": points},
        "instances": instances,
    })
}

/// What a server's card shows, from a heartbeat's `host` (or this host's
/// sample): every field optional, as older agents send fewer.
fn card(host: &Value) -> Value {
    let mut c = serde_json::Map::new();
    for k in [
        "hostname",
        "cpus",
        "cpu_pct",
        "cpu_history",
        "mem_used",
        "mem_total",
        "disk_used",
        "disk_total",
        "net_rx_rate",
        "net_tx_rate",
        "load1",
    ] {
        if let Some(v) = host.get(k) {
            c.insert(k.into(), v.clone());
        }
    }
    Value::Object(c)
}

/// A heartbeat's `host` in the monitor's shape: what it lacks (per-core
/// CPU, pools, interfaces) left empty rather than missing.
fn heartbeat_host(host: &Value, heartbeat: &Value) -> Value {
    let mut v = serde_json::to_value(crate::metrics::HostSample::default()).unwrap_or_default();
    if let (Some(m), Some(h)) = (v.as_object_mut(), host.as_object()) {
        m.extend(h.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    v["isb"] = heartbeat["isb"].clone();
    v
}

fn answer(d: &super::Daemon, a: &Value) -> Result<Value> {
    let range = range(a);
    let me = d.ctl.snapshot().host;
    let mut servers = vec![json!({
        "name": me.hostname,
        "local": true,
        "kind": "local",
        "vm_org": null,
        "state": "up",
        "last_ok": null,
        "host": card(&serde_json::to_value(&me).unwrap_or_default()),
    })];
    let records = d.servers.as_ref().map(|s| s.records()).unwrap_or_default();
    for r in &records {
        let h = d
            .servers
            .as_ref()
            .map(|s| s.health(&r.name))
            .unwrap_or_default();
        let host = h.heartbeat.get("host").filter(|v| v.is_object());
        servers.push(json!({
            "name": r.name,
            "local": false,
            "kind": if r.vm.is_some() { "vm" } else { "ssh" },
            "vm_org": r.vm.as_ref().map(|v| v.org.to_string()),
            "state": h.state,
            "last_ok": h.last_ok,
            "host": host.map(card),
        }));
    }
    let want = a
        .get("server")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let remote = want.filter(|w| records.iter().any(|r| r.name == *w));
    let (Some(name), Some(s)) = (remote, &d.servers) else {
        if let Some(w) = want.filter(|w| *w != me.hostname) {
            return Err(Error::NotFound(format!("server {w}")));
        }
        return Ok(json!({
            "servers": servers,
            "server": me.hostname,
            "monitor": local(&d.ctl, range),
            "error": null,
            "partial": false,
        }));
    };
    let h = s.health(name);
    let (monitor, error, partial) = match s.client(name).and_then(|c| {
        c.internal(
            "POST",
            "/internal/v1/monitor",
            Some(&json!({"range": range})),
            REMOTE_TIMEOUT,
        )
    }) {
        Ok(m) => (m, Value::Null, false),
        Err(e) if e.is_not_found() => match h.heartbeat.get("host").filter(|v| v.is_object()) {
            Some(host) => (
                json!({"at": h.last_ok.unwrap_or(0) * 1000, "host": heartbeat_host(host, &h.heartbeat), "history": {"step": 0, "points": []}, "instances": []}),
                json!("this server's isb is too old for live detail: upgrade it to see it"),
                true,
            ),
            None => (Value::Null, json!(e.to_string()), false),
        },
        Err(e) => (Value::Null, json!(e.to_string()), false),
    };
    Ok(json!({
        "servers": servers,
        "server": name,
        "monitor": monitor,
        "error": error,
        "partial": partial,
    }))
}

pub(super) fn register(r: &mut Registry, d: Arc<super::Daemon>) -> Result<()> {
    r.register(
        Tool::new(
            "host_monitor",
            "Live resource use of this host or one remote server, as `top`/`bottom` show it: CPU (overall and per core), load, uptime, memory and swap, each storage pool, disk I/O and every network interface with its addresses and rates, the last `range` seconds (60-3600, default 300) of CPU, memory and network, and every instance's CPU, memory, network and disk rates. `servers` has a card per server (this host first) with its health and latest heartbeat numbers; `server` picks a remote one by name. Superadmins only.",
            json!({"type": "object", "properties": {
                "server": {"type": "string", "description": "A remote server's name; omitted: this host"},
                "range": {"type": "integer", "minimum": 60, "maximum": 3600, "description": "Seconds of history (default 300)"},
                "org": {"type": "string"},
            }, "additionalProperties": false}),
            move |a, _c| answer(&d, &a),
        )
        .title("Host monitor")
        .annotations(json!({"readOnlyHint": true, "openWorldHint": false})),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_heartbeat_fills_the_monitor_shape() {
        let hb = json!({"isb": "1.6.0", "host": {"hostname": "b", "cpus": 4, "mem_used": 1, "mem_total": 2}});
        let h = heartbeat_host(&hb["host"], &hb);
        assert_eq!(h["hostname"], "b");
        assert_eq!(h["cpus"], 4);
        assert_eq!(h["isb"], "1.6.0");
        for k in ["cpu_cores", "pools", "interfaces"] {
            assert_eq!(h[k], json!([]), "{k}");
        }
        assert!(h["swap_total"].is_null());
    }

    #[test]
    fn range_is_clamped() {
        assert_eq!(range(&json!({})), DEFAULT_RANGE);
        assert_eq!(range(&json!({"range": 5})), 60);
        assert_eq!(range(&json!({"range": 99999})), 3600);
        assert_eq!(range(&json!({"range": 900})), 900);
    }
}
