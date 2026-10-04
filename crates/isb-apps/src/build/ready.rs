//! Waiting for a build instance to be ready: to run commands, and to have a
//! network. The images isb builds on the host's own bridge (`incusbr0`)
//! download everything, and a host firewall that drops DHCP leaves the
//! container without an address, which would otherwise look like a build
//! that never makes progress.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::exec::ExecOptions;
use crate::sandbox::Sandbox;

/// Wait until commands run (a VM: once its agent is up).
pub(super) fn exec(sb: &Sandbox, deadline: Instant) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(300);
    let mut last = String::new();
    while Instant::now() < until.min(deadline) {
        match sb
            .exec_stream(
                ["/bin/true"],
                ExecOptions::default().timeout(Duration::from_secs(20)),
            )
            .and_then(|s| s.collect_output())
        {
            Ok(o) if o.success() => return Ok(()),
            Ok(o) => last = format!("exit {}", o.exit_code),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(Error::invalid(format!(
        "build sandbox {} did not accept commands: {last}",
        sb.name()
    )))
}

/// How long a prepared instance on the host's bridge may take to get an
/// IPv4 address before the build gives up.
const NETWORK_WAIT: Duration = Duration::from_secs(90);

/// The instance's first global IPv4 address, from its state.
fn ipv4_of(state: &Value) -> Option<String> {
    state["network"]
        .as_object()?
        .iter()
        .filter(|(nic, _)| nic.as_str() != "lo")
        .flat_map(|(_, n)| n["addresses"].as_array().into_iter().flatten())
        .find(|a| a["family"] == "inet" && a["scope"] == "global")
        .and_then(|a| a["address"].as_str().map(String::from))
}

/// What a build says when its container gets no address.
fn no_network_message(net: &str, waited: Duration) -> String {
    format!(
        "the build container got no IPv4 address on {net} within {}s, so nothing it runs can reach the network. \
A default-deny host firewall (ufw) is the usual cause: run `sudo isb host setup`, which lets {net} through (DHCP, DNS, egress), then build again. \
Otherwise check that `incus network show {net}` has an ipv4.address and that the host forwards IPv4",
        waited.as_secs()
    )
}

/// Poll `state` until the instance has an IPv4 address; the address, or the
/// "no network" error after `within`.
fn wait_for_address(
    state: &mut dyn FnMut() -> Result<Value>,
    net: &str,
    within: Duration,
    poll: Duration,
) -> Result<String> {
    let until = Instant::now() + within;
    loop {
        if let Some(a) = state().ok().as_ref().and_then(ipv4_of) {
            return Ok(a);
        }
        if Instant::now() >= until {
            return Err(Error::invalid(no_network_message(net, within)));
        }
        std::thread::sleep(poll);
    }
}

/// Wait for a started instance on `net` to get an IPv4 address, so a build
/// that downloads things fails fast and clearly where there is no network
/// instead of waiting on a download that never starts.
pub(super) fn network(
    client: &Client,
    name: &str,
    net: &str,
    deadline: Instant,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    let path = format!("/1.0/instances/{}/state", encode_segment(name));
    let within = NETWORK_WAIT.min(deadline.saturating_duration_since(Instant::now()));
    let a = wait_for_address(
        &mut || client.get(&path),
        net,
        within,
        Duration::from_secs(2),
    )?;
    log(&format!("{name} has {a} on {net}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_container_without_an_address_fails_fast_with_the_way_out() {
        let none = json!({"network": {"lo": {"addresses": [{"family": "inet", "address": "127.0.0.1", "scope": "local"}]}, "eth0": {"addresses": [{"family": "inet6", "address": "fe80::1", "scope": "link"}]}}});
        assert_eq!(ipv4_of(&none), None);
        assert_eq!(ipv4_of(&json!({"network": null})), None);
        let up = json!({"network": {"eth0": {"addresses": [{"family": "inet", "address": "10.9.9.7", "scope": "global"}]}}});
        assert_eq!(ipv4_of(&up).as_deref(), Some("10.9.9.7"));
        // Never an address: the error names the bridge and `isb host setup`.
        let mut polls = 0;
        let e = wait_for_address(
            &mut || {
                polls += 1;
                Ok(none.clone())
            },
            "incusbr0",
            Duration::from_millis(30),
            Duration::from_millis(10),
        )
        .unwrap_err()
        .to_string();
        assert!(polls >= 2, "{polls}");
        assert!(e.contains("no IPv4 address on incusbr0"), "{e}");
        assert!(e.contains("sudo isb host setup"), "{e}");
        // An address that arrives late is found; errors while starting are retried.
        let mut n = 0;
        let a = wait_for_address(
            &mut || {
                n += 1;
                match n {
                    1 => Err(Error::invalid("not up yet")),
                    2 => Ok(none.clone()),
                    _ => Ok(up.clone()),
                }
            },
            "incusbr0",
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(a, "10.9.9.7");
    }
}
