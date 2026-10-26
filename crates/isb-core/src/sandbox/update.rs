//! Changing an instance's config and devices safely under concurrent writers.

use serde_json::{Value, json};

use super::inst_path;
use crate::client::Client;
use crate::error::{Error, Result};

type Obj = serde_json::Map<String, Value>;

/// Read-modify-write of an instance under `If-Match`, so a concurrent change by
/// another tool is never silently overwritten (a 412 re-reads and retries).
#[doc(hidden)]
pub fn update_instance(
    client: &Client,
    name: &str,
    step: &str,
    modify: &mut dyn FnMut(&mut Obj, &mut Obj) -> Result<()>,
) -> Result<()> {
    let path = inst_path(name);
    for attempt in 0..5 {
        let (inst, etag) = client.get_etag(&path)?;
        let mut config = inst
            .get("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut devices = inst
            .get("devices")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        modify(&mut config, &mut devices)?;
        let body = json!({
            "architecture": inst.get("architecture"),
            "config": config,
            "devices": devices,
            "ephemeral": inst.get("ephemeral"),
            "profiles": inst.get("profiles"),
            "stateful": inst.get("stateful"),
            "description": inst.get("description"),
        });
        match client.mutate_if_match(
            "PUT",
            &path,
            &body,
            etag.as_deref(),
            step,
            client.timeouts.other,
        ) {
            Err(Error::Api { status: 412, .. }) if attempt < 4 => continue,
            r => return r.map(|_| ()),
        }
    }
    unreachable!()
}
