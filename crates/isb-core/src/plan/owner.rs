//! Who owns a named volume's mount point. A new volume gets numeric ids and
//! a mode at creation (incus' `initial.uid`/`initial.gid`/`initial.mode`,
//! before anything runs in the instance); `owner` and `mode` are then applied
//! from the host once the mount is there, resolving names in the image.
//!
//! With no `owner`, a new volume belongs to the service's `user`, as a
//! process running as that user expects of its data directory. That never
//! overrides a volume the image seeded (`initial.copy` copies the image's
//! owner and mode), and never touches an existing volume.

use super::{Action, HostFacts, OwnerFixup, Props};
use crate::error::{Error, Result};
use crate::owner::{numeric_ids, parse_mode};
use crate::spec::{SandboxSpec, VolumeSpec};

/// The config to create the named volume `v` mounts with (its definition's,
/// plus `initial.*` the definition does not set), and the fixups to run once
/// it is attached at `path` as `device`.
pub(super) fn for_mount(
    name: &str,
    spec: &SandboxSpec,
    v: &VolumeSpec,
    (path, device): (&str, &str),
    volume: (&str, &str, Props),
    host: &HostFacts,
) -> Result<(Props, Vec<OwnerFixup>)> {
    let mode = v
        .mode
        .as_deref()
        .map(parse_mode)
        .transpose()
        .map_err(|e| Error::invalid(format!("{name}: {}: {e}", v.target)))?;
    let fixup = |owner: Option<&String>, mode: Option<u32>, new_volume: bool| OwnerFixup {
        device: device.into(),
        path: path.into(),
        owner: owner.cloned(),
        mode: mode.map(|m| format!("{m:04o}")),
        new_volume: new_volume.then(|| (volume.0.to_string(), volume.1.to_string())),
    };
    let mut initial = Props::new();
    let mut fixups = Vec::new();
    let implicit = v.owner.is_none().then_some(spec.user.as_ref()).flatten();
    if let Some((uid, gid)) = v.owner.as_ref().or(implicit).and_then(|o| numeric_ids(o)) {
        initial.insert("initial.uid".into(), uid.to_string());
        initial.insert("initial.gid".into(), gid.to_string());
    }
    if let Some(m) = mode {
        initial.insert("initial.mode".into(), format!("{m:04o}"));
    }
    if !host.initial_owner {
        initial.clear();
    }
    if let Some(o) = &v.owner {
        fixups.push(fixup(Some(o), mode, false));
    } else {
        if let Some(u) = implicit.filter(|_| !initial.contains_key("initial.uid")) {
            fixups.push(fixup(Some(u), None, true));
        }
        if mode.is_some() {
            fixups.push(fixup(None, mode, false));
        }
    }
    let mut config = volume.2;
    for (k, val) in initial {
        config.entry(k).or_insert(val);
    }
    Ok((config, fixups))
}

/// How a plan shows a fixup.
pub(super) fn describe(
    f: &mut std::fmt::Formatter<'_>,
    path: &str,
    owner: Option<&str>,
    mode: Option<&str>,
    fresh_only: bool,
) -> std::fmt::Result {
    match (owner, mode) {
        (Some(o), Some(m)) => write!(f, "~ chown {o} {path}, chmod {m}")?,
        (Some(o), None) => write!(f, "~ chown {o} {path}")?,
        (None, m) => write!(f, "~ chmod {} {path}", m.unwrap_or("-"))?,
    }
    if fresh_only {
        write!(f, " (new volume the image did not seed)")?;
    }
    Ok(())
}

/// The fixups to run: a mount's own when its device is new, the service
/// user's when its volume is created now as well.
pub(super) fn actions(
    owners: &[OwnerFixup],
    new_device: &dyn Fn(&str) -> bool,
    volumes_missing: &[(String, String)],
) -> Vec<Action> {
    owners
        .iter()
        .filter(|o| new_device(&o.device))
        .filter(|o| {
            o.new_volume
                .as_ref()
                .is_none_or(|v| volumes_missing.contains(v))
        })
        .map(|o| Action::FixOwner {
            path: o.path.clone(),
            owner: o.owner.clone(),
            mode: o.mode.clone(),
            fresh_only: o.new_volume.is_some(),
        })
        .collect()
}
