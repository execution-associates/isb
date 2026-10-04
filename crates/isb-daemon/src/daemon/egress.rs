//! The daemon's side of sandbox egress: the proxy manager, and the secret
//! store it reads real values from (docs/guides/egress.md).

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use isb_egress::{Env, Manager, SecretSource, Settings};

use super::ServeConfig;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::spec::SandboxSpec;

/// The org store a sandbox in incus project `project` reads secrets from:
/// its own org's, and the default org's for a sandbox outside every org.
struct Store(Arc<Secrets>);

impl SecretSource for Store {
    fn get(&self, project: &str, name: &str) -> std::result::Result<Vec<u8>, String> {
        let org = OrgId::from_incus_project(project).unwrap_or_else(OrgId::default_org);
        self.0
            .get(&org, name)
            .map(|(v, _)| v)
            .map_err(|e| e.to_string())
    }
}

/// `--egress-pin name=ip[:port]`.
fn parse_pin(s: &str) -> Result<(String, SocketAddr)> {
    let (name, addr) = s
        .split_once('=')
        .ok_or_else(|| Error::invalid(format!("--egress-pin {s:?}: expected NAME=IP[:PORT]")))?;
    let with_port = if addr.contains(':') && !addr.starts_with('[') && addr.matches(':').count() == 1 {
        addr.to_string()
    } else if addr.starts_with('[') {
        addr.to_string()
    } else {
        format!("{addr}:0")
    };
    let sa = with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or_else(|| Error::invalid(format!("--egress-pin {s:?}: {addr:?} is not an address")))?;
    Ok((name.trim().to_ascii_lowercase(), sa))
}

/// Start the proxy manager: one proxy per egress network incus holds.
pub(super) fn start(
    cfg: &ServeConfig,
    client: &Client,
    secrets: &Arc<Secrets>,
) -> Result<(Arc<Manager>, Arc<AtomicBool>)> {
    crate::egress::ca::set_state_dir(&cfg.state_dir);
    let mut settings = Settings::default();
    for p in &cfg.egress_pins {
        let (n, a) = parse_pin(p)?;
        settings.pin(&n, a);
    }
    for f in &cfg.egress_ca {
        let n = settings
            .trust_file(f)
            .map_err(|e| Error::invalid(format!("--egress-ca {}: {e}", f.display())))?;
        if n == 0 {
            return Err(Error::invalid(format!(
                "--egress-ca {}: no certificates in it",
                f.display()
            )));
        }
    }
    let env = Arc::new(Env {
        settings: Arc::new(settings),
        secrets: Arc::new(Store(secrets.clone())),
        log: Arc::new(|l| eprintln!("isb serve: {l}")),
    });
    let manager = Manager::new(client.clone(), env);
    let stop = Arc::new(AtomicBool::new(false));
    manager.spawn(stop.clone());
    Ok((manager, stop))
}

/// Refuse a sandbox whose egress names a secret the org does not hold, so
/// the mistake shows when the sandbox is made, not on its first request.
pub(super) fn check_secrets(secrets: &Secrets, org: &OrgId, spec: &SandboxSpec) -> Result<()> {
    let Some(e) = &spec.egress else {
        return Ok(());
    };
    for s in &e.secrets {
        let name = s.secret.as_deref().unwrap_or(&s.env);
        secrets.inspect(org, name).map_err(|err| {
            if err.is_not_found() {
                Error::invalid(format!(
                    "egress secret {}: org {org} has no secret named {name}",
                    s.env
                ))
            } else {
                err
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_parse() {
        let (n, a) = parse_pin("Svc.Example.com=10.1.2.3:8443").unwrap();
        assert_eq!(n, "svc.example.com");
        assert_eq!(a, "10.1.2.3:8443".parse().unwrap());
        let (_, a) = parse_pin("x.example.com=127.0.0.1").unwrap();
        assert_eq!(a.port(), 0, "no port: the port the guest asked for");
        assert!(parse_pin("nonsense").is_err());
        assert!(parse_pin("a.example.com=notanip").is_err());
    }
}
