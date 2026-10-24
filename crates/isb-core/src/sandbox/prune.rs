//! Deleting sandboxes whose host directory is gone.

use std::path::Path;

use serde::Serialize;

use super::{LabelFilter, Reporter, Sandbox, force_delete};
use crate::client::Client;
use crate::error::Result;

/// One instance `prune` considered.
#[derive(Debug, Clone, Serialize)]
pub struct PruneItem {
    pub name: String,
    pub path: String,
    pub deleted: bool,
}

/// Delete instances whose `label` value is a host path that no longer exists.
/// Instances without the label, or whose path exists, are never touched.
/// With `dry_run`, nothing is deleted.
pub fn prune_missing_path(
    client: &Client,
    label: &str,
    dry_run: bool,
    report: Reporter<'_>,
) -> Result<Vec<PruneItem>> {
    let mut out = Vec::new();
    for i in Sandbox::list_with(client, &[LabelFilter::parse(label)])? {
        let Some(path) = i.labels.get(label) else {
            continue;
        };
        if path.is_empty() || !path.starts_with('/') || Path::new(path).exists() {
            continue;
        }
        if dry_run {
            report(&format!(
                "would delete {}  ({label}={path} is gone)",
                i.name
            ));
        } else {
            report(&format!("deleting {}  ({label}={path} is gone)", i.name));
            force_delete(client, &i.name)?;
        }
        out.push(PruneItem {
            name: i.name,
            path: path.clone(),
            deleted: !dry_run,
        });
    }
    Ok(out)
}
