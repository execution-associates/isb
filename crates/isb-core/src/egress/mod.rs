//! Per-sandbox egress policy: an allowlist of `host[:port]` a sandbox may
//! reach, and secrets that never enter the guest.
//!
//! The pieces, in the order a connection meets them (docs/guides/egress.md):
//!
//! - [`policy`]: the `egress:` field, validated, and the matching rules;
//! - [`plumb`]: the incus side, a bridge, an ACL and a filtering dnsmasq per
//!   sandbox, so the guest can reach nothing but the proxy;
//! - [`ca`]: the per-sandbox CA the proxy terminates TLS with, and the
//!   guest's trust in it;
//! - the proxy itself, which enforces the policy, is the `isb-egress` crate,
//!   run by `isb serve`.

pub mod ca;
pub mod names;
pub mod plumb;
pub mod policy;
pub mod presets;

pub use plumb::Plumbing;
pub use policy::{EgressSecretSpec, EgressSpec, Entry, HostPattern, Policy, SecretBinding};

use std::time::{Duration, Instant};

use crate::client::Client;
use crate::error::Result;
use plumb::{KEY_POLICY, Props};

/// What a sandbox's `egress:` adds to its resolved instance.
#[derive(Debug, Clone)]
pub struct Contribution {
    pub plumbing: Plumbing,
    pub nic: Props,
    pub config: Props,
}

/// Validate `spec` for instance `instance` in incus project `project`, and
/// say what it adds: the NIC, and the config keys (the stored policy, the
/// placeholders, and the CA settings when a secret is involved).
pub fn contribute(spec: &EgressSpec, project: &str, instance: &str) -> Result<Contribution> {
    let network = plumb::network_name(project, instance);
    let policy = Policy::from_spec(spec, &network)?;
    let plumbing = Plumbing::new(project, instance, policy);
    let mut config = Props::new();
    config.insert(KEY_POLICY.into(), plumbing.policy_json());
    config.insert(plumb::KEY_FOR.into(), plumbing.owner());
    for s in &plumbing.policy.secrets {
        config.insert(format!("environment.{}", s.env), s.placeholder.clone());
    }
    if !plumbing.policy.secrets.is_empty() {
        config.extend(ca::guest_env());
    }
    Ok(Contribution {
        nic: plumbing.nic(),
        plumbing,
        config,
    })
}

/// Before the instance is created or changed: the bridge, the ACL and the CA.
pub fn before_apply(client: &Client, p: &Plumbing, report: &mut dyn FnMut(&str)) -> Result<()> {
    // The CA first: the proxy looks for it as soon as the network exists.
    if !p.policy.secrets.is_empty() {
        ca::Ca::ensure(&p.network)?;
    }
    plumb::prepare(client, p, report)?;
    Ok(())
}

/// How long a started sandbox may take to resolve an allowed name.
const RESOLVER_WAIT: Duration = Duration::from_secs(60);

/// Wait until the guest resolves one of the policy's names. For about a
/// second after a container starts, /etc/resolv.conf does not exist yet
/// (systemd-resolved writes it) and the DHCP lease from the egress bridge
/// may not have arrived, so the first command of a fresh sandbox would fail
/// DNS. An image without `getent` (exit 126/127) is not waited for.
pub fn wait_resolver(client: &Client, instance: &str, policy: &Policy) -> Result<()> {
    let Some(name) = policy.probe_name() else {
        return Ok(());
    };
    let argv = ["getent".to_string(), "hosts".into(), name.clone()];
    let until = Instant::now() + RESOLVER_WAIT;
    let mut last = String::new();
    while Instant::now() < until {
        match crate::exec::run_captured(
            client,
            instance,
            &argv,
            &crate::exec::Request::default(),
            crate::exec::Stdin::Null,
            Some(Duration::from_secs(20)),
        ) {
            Ok(o) if o.success() || matches!(o.exit_code, 126 | 127) => return Ok(()),
            Ok(o) => last = format!("getent exited {}", o.exit_code),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(crate::error::Error::invalid(format!(
        "{instance}: the guest could not resolve {name} through its egress DNS within {}s ({last}); \
check that /etc/resolv.conf names a nameserver and that `isb serve` is running",
        RESOLVER_WAIT.as_secs()
    )))
}

/// After an existing sandbox starts again: its DNS races just like a new
/// one's. `config` is the instance's config, which holds the policy.
pub fn after_start(
    client: &Client,
    instance: &str,
    config: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    match stored_policy(config) {
        Some(p) if !p.is_none() => wait_resolver(client, instance, &p),
        _ => Ok(()),
    }
}

/// The policy a running instance carries in its config, if any.
pub fn stored_policy(config: &std::collections::BTreeMap<String, String>) -> Option<Policy> {
    serde_json::from_str(config.get(KEY_POLICY)?).ok()
}

/// Once the instance runs: wait for its DNS, then make the guest trust the
/// CA, when there are secrets.
pub fn after_ready(client: &Client, p: &Plumbing) -> Result<()> {
    if !p.policy.is_none() {
        wait_resolver(client, &p.instance, &p.policy)?;
    }
    if p.policy.secrets.is_empty() {
        return Ok(());
    }
    match ca::Ca::load(&p.network)? {
        Some(ca) => ca::install(client, &p.instance, &ca),
        None => Err(crate::error::Error::invalid(format!(
            "{}: the egress CA is missing from this host's state directory; recreate the sandbox",
            p.instance
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_policy_reads_back_for_a_restart() {
        let spec = EgressSpec::allow(["registry.npmjs.org", "*.githubusercontent.com"]);
        let c = contribute(&spec, "default", "web").unwrap();
        let config = c
            .config
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let p = stored_policy(&config).expect("the policy parses");
        assert_eq!(p, c.plumbing.policy);
        assert!(!p.is_none());
        assert!(stored_policy(&Default::default()).is_none());
    }
}
