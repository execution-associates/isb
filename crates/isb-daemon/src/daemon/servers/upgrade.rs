//! `server_upgrade`: replace servers' agents with this control plane's
//! build, or a release, and wait for each to answer with it
//! ([`crate::servers::upgrade`]).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, platform_only, servers};
use crate::error::{Error, Result};
use crate::server::{Caller, Registry, Tool};
use crate::servers::upgrade::Source;

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    r.register(
        Tool::new(
            "server_upgrade",
            "Platform admins: upgrade a server's agent (name) or every server's (all: true) to this control plane's own build, a release (version, checked against its SHA256SUMS) or a Linux binary on this host (isb_binary, local CLI only). The binary goes over the agent's mTLS connection (a dedicated VM: through incus); a root helper on the box checks its SHA-256, installs it with the old one kept, restarts the agent, and puts the old one back unless this control plane sees the new build answer within 120 s. Each takes a minute or two; calls for the server's orgs fail while its agent restarts, its workloads keep running.",
            json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "The server."},
                "all": {"type": "boolean", "description": "Every server, one after another."},
                "version": {"type": "string", "description": "The isb release to install (default: this control plane's own binary)."},
                "isb_binary": {"type": "string", "description": "A Linux isb binary on this host (local CLI only)."}
            }, "additionalProperties": false}),
            move |a: Value, c: &Caller| upgrade(&d, a, c),
        )
        .title("Upgrade servers")
        .annotations(json!({"destructiveHint": false, "openWorldHint": true})),
    )
}

fn upgrade(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: Option<String>,
        #[serde(default)]
        all: bool,
        version: Option<String>,
        isb_binary: Option<PathBuf>,
    }
    platform_only(c)?;
    let s = servers(d)?;
    let a: A = super::args(a)?;
    if a.isb_binary.is_some() && !c.is_trusted() {
        return Err(Error::Forbidden(
            "isb_binary is a path on the control plane's host: local CLI only (or give a release version)".into(),
        ));
    }
    let source = match (a.version, a.isb_binary) {
        (Some(_), Some(_)) => return Err(Error::invalid("give version or isb_binary, not both")),
        (Some(v), None) => Source::Release(v.trim_start_matches('v').to_string()),
        (None, Some(f)) => Source::File(f),
        (None, None) => Source::Own,
    };
    let names: Vec<String> = match (a.name, a.all) {
        (Some(n), false) => {
            s.record(&n)?;
            return s.upgrade(&d.client, &n, &source);
        }
        (None, true) => s.records().into_iter().map(|r| r.name).collect(),
        _ => return Err(Error::invalid("give a server's name, or all: true")),
    };
    let results: Vec<Value> = names
        .iter()
        .map(|n| match s.upgrade(&d.client, n, &source) {
            Ok(v) => v,
            Err(e) => json!({"name": n, "upgraded": false, "error": e.to_string()}),
        })
        .collect();
    let failed = results.iter().filter(|r| r.get("error").is_some()).count();
    Ok(json!({"servers": results, "failed": failed}))
}
