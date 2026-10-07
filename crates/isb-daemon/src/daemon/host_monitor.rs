//! `host_monitor`: the host's live numbers (CPU per core, memory and swap,
//! pools, disk I/O, interfaces), its last hour, and every instance's rates.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::Result;
use crate::server::{Registry, Tool};

/// Chart points per answer: enough for a wide chart, few enough to poll.
const MAX_POINTS: usize = 300;

pub const DEFAULT_RANGE: u64 = 300;

/// Seconds of history asked for, clamped to what the sampler keeps.
pub fn range(a: &Value) -> u64 {
    a.get("range")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_RANGE)
        .clamp(60, 3600)
}

/// This host's monitor answer.
fn local(ctl: &crate::stack::Controller, range: u64) -> Value {
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

pub(super) fn register(r: &mut Registry, d: Arc<super::Daemon>) -> Result<()> {
    r.register(
        Tool::new(
            "host_monitor",
            "Live resource use of this host, as `top`/`bottom` show it: CPU (overall and per core), load, uptime, memory and swap, each storage pool, disk I/O and every network interface with its addresses and rates, the last `range` seconds (60-3600, default 300) of CPU, memory and network, and every instance's CPU, memory, network and disk rates. Superadmins only.",
            json!({"type": "object", "properties": {
                "range": {"type": "integer", "minimum": 60, "maximum": 3600, "description": "Seconds of history (default 300)"},
                "org": {"type": "string"},
            }, "additionalProperties": false}),
            move |a, _c| Ok(local(&d.ctl, range(&a))),
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
    fn range_is_clamped() {
        assert_eq!(range(&json!({})), DEFAULT_RANGE);
        assert_eq!(range(&json!({"range": 5})), 60);
        assert_eq!(range(&json!({"range": 99999})), 3600);
        assert_eq!(range(&json!({"range": 900})), 900);
    }
}
