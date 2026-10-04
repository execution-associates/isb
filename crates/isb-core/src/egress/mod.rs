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
pub mod plumb;
pub mod policy;

pub use plumb::Plumbing;
pub use policy::{EgressSecretSpec, EgressSpec, Entry, HostPattern, Policy, SecretBinding};

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

/// Once the instance runs: make the guest trust the CA, when there are secrets.
pub fn after_ready(client: &Client, p: &Plumbing) -> Result<()> {
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
