//! Named custom storage volumes.

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::plan::Props;

/// A custom volume as incus reports it.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    pub name: String,
    pub pool: String,
    pub content_type: String,
    pub config: Props,
    /// Instances (and profiles) using it, as incus URLs.
    pub used_by: Vec<String>,
}

impl VolumeInfo {
    fn from_api(pool: &str, v: &Value) -> VolumeInfo {
        VolumeInfo {
            name: v.get("name").and_then(Value::as_str).unwrap_or("").into(),
            pool: pool.into(),
            content_type: v.get("content_type").and_then(Value::as_str).unwrap_or("").into(),
            config: v
                .get("config")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            used_by: v
                .get("used_by")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }
}

fn vol_path(pool: &str, name: &str) -> String {
    format!(
        "/1.0/storage-pools/{}/volumes/custom/{}",
        encode_segment(pool),
        encode_segment(name)
    )
}

/// All custom volumes in `pool`.
pub fn list(client: &Client, pool: &str) -> Result<Vec<VolumeInfo>> {
    let v = client.get(&format!(
        "/1.0/storage-pools/{}/volumes/custom?recursion=1",
        encode_segment(pool)
    ))?;
    let mut out: Vec<VolumeInfo> = v
        .as_array()
        .map(|a| a.iter().map(|x| VolumeInfo::from_api(pool, x)).collect())
        .unwrap_or_default();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn get(client: &Client, pool: &str, name: &str) -> Result<Option<VolumeInfo>> {
    Ok(client
        .get_opt(&vol_path(pool, name))?
        .map(|v| VolumeInfo::from_api(pool, &v)))
}

/// Create if missing. Returns true if this call created it.
pub fn ensure(client: &Client, pool: &str, name: &str, config: &Props) -> Result<bool> {
    if get(client, pool, name)?.is_some() {
        return Ok(false);
    }
    let body = json!({"name": name, "type": "custom", "content_type": "filesystem", "config": config});
    match client.mutate(
        "POST",
        &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(pool)),
        Some(&body),
        &format!("create volume {name}"),
        client.timeouts.other,
    ) {
        Ok(_) => Ok(true),
        // Lost a race with another creator: fine, as long as it exists now.
        Err(_) if get(client, pool, name)?.is_some() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Delete a volume. Refuses while an instance uses it.
pub fn remove(client: &Client, pool: &str, name: &str) -> Result<()> {
    let Some(v) = get(client, pool, name)? else {
        return Err(Error::NotFound(format!("volume {name} in pool {pool}")));
    };
    if !v.used_by.is_empty() {
        return Err(Error::invalid(format!(
            "volume {name} is in use by {}",
            v.used_by.join(", ")
        )));
    }
    client
        .mutate("DELETE", &vol_path(pool, name), None, &format!("delete volume {name}"), client.timeouts.other)
        .map(|_| ())
}
