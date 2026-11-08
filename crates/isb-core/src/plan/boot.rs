//! What a spec says about the host's boot and disk: the `boot.autostart`
//! keys an instance gets, and a dev sandbox's root disk I/O caps.

use super::Props;
use crate::flex::parse_duration;
use crate::spec::{RestartMode, SandboxSpec};

/// Seconds incus waits after autostarting a container, or a VM, before it
/// starts the next one. Incus starts autostart instances one at a time, but
/// a start returns as soon as the guest's init runs, so with no delay a host
/// boot wakes every instance in the same few seconds (2026-10-24 on titan:
/// 154 at once, io pressure at 98% for 85 minutes).
pub const BOOT_DELAY_CONTAINER: u64 = 2;
pub const BOOT_DELAY_VM: u64 = 10;

/// The `boot.autostart` keys an instance gets: the spec's `boot`, over the
/// defaults. `always` and `on-failure` start with the host, spaced by
/// [`BOOT_DELAY_CONTAINER`] (or `_VM`); `unless-stopped` leaves autostart
/// unset, which in incus restores the state the instance had at shutdown;
/// anything else is `false`, so a reboot does not bring back every dev
/// sandbox that happened to be running. Applied in place by the stack
/// controller too, so a replica is never replaced for these keys.
///
/// `boot.autorestart` (incus restarting an instance that exited) is on for a
/// long-running sandbox and off for a stack replica, whose controller is its
/// one supervisor: with both reacting to an exit, two starts of one instance
/// overlapped, and one came up with a `/etc/resolv.conf` that named no
/// nameserver (2026-11-05 on titan, DNS lost for 75 hours).
pub fn boot_config(spec: &SandboxSpec, vm: bool) -> Result<Props, String> {
    let b = spec.boot.clone().unwrap_or_default();
    let autostart = b.autostart.or(match spec.restart.unwrap_or_default() {
        RestartMode::Always | RestartMode::OnFailure => Some(true),
        RestartMode::UnlessStopped => None,
        RestartMode::No => Some(false),
    });
    let mut out = Props::new();
    if is_replica(spec) {
        out.insert("boot.autorestart".into(), "false".into());
    } else if spec.restart.is_some_and(|r| r.is_long_running()) {
        out.insert("boot.autorestart".into(), "true".into());
    }
    if let Some(a) = autostart {
        out.insert("boot.autostart".into(), a.to_string());
    }
    if let Some(p) = b.priority {
        out.insert("boot.autostart.priority".into(), p.to_string());
    }
    let delay = match &b.delay {
        Some(d) => Some(
            parse_duration(d)
                .map_err(|e| format!("boot.delay: {e}"))?
                .as_secs(),
        ),
        None if autostart == Some(true) => Some(if vm {
            BOOT_DELAY_VM
        } else {
            BOOT_DELAY_CONTAINER
        }),
        None => None,
    };
    if let Some(d) = delay {
        out.insert("boot.autostart.delay".into(), d.to_string());
    }
    Ok(out)
}

/// A stack replica: the controller labels every instance it creates.
pub(super) fn is_replica(spec: &SandboxSpec) -> bool {
    spec.labels.contains_key(crate::stack::LABEL_STACK)
}

/// The root disk's default I/O caps for a sandbox that is not a stack
/// replica, merged under `raw_devices.root`. They bound one runaway sandbox
/// (a build, a test suite, a stuck loop) on a host whose instances share one
/// disk, without starving it: titan-iac measured a 2MiB/s cap breaking a
/// k3s control plane, and caps its org guests at 128MiB/s read and 32MiB/s
/// write. A dev sandbox writes more (cargo targets, package installs), so
/// its write cap is twice that.
pub fn dev_disk_limits() -> Props {
    Props::from([
        ("limits.read".into(), "128MiB".into()),
        ("limits.write".into(), "64MiB".into()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replica_is_never_restarted_by_incus() {
        let mut s = SandboxSpec::new("web", "docker:nginx:1.27");
        s.restart = Some(RestartMode::Always);
        assert_eq!(boot_config(&s, false).unwrap()["boot.autorestart"], "true");
        s.labels
            .insert(crate::stack::LABEL_STACK.into(), "shop".into());
        let b = boot_config(&s, false).unwrap();
        // The stack controller restarts it, alone.
        assert_eq!(b["boot.autorestart"], "false");
        // It still starts with the host.
        assert_eq!(b["boot.autostart"], "true");
        s.labels.clear();
        s.restart = Some(RestartMode::No);
        assert!(
            !boot_config(&s, false)
                .unwrap()
                .contains_key("boot.autorestart")
        );
    }
}
