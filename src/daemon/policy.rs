//! What a remote caller may ask for.
//!
//! The incus socket is root on the host, and `isb serve` holds it. A caller
//! through Cloudflare Access is trusted to run workloads, not to own the host,
//! so its compose files and sandbox specs are checked here before anything
//! reaches incus. Local callers (the unix socket, the same user as the daemon)
//! skip all of this: they could run isb directly.
//!
//! Refused unless the operator opts in:
//! - `privileged`, `raw_config`, `raw_devices`, `incus_profiles`, a custom
//!   `idmap` map, and guest-bound proxies (a guest reaching into the host);
//! - bind mounts outside `--bind-root` directories (none by default), and
//!   symlinks that lead out of them;
//! - publishing on anything but loopback, unless the address is listed in
//!   `--publish-address`; unix-socket listeners;
//! - a different `incus_project`;
//! - secrets read from host files (pass their values instead).
//!
//! Interpolation for a remote caller sees only the variables it sent, never
//! the daemon's environment.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::spec::{ComposeFile, IdmapMode, IdmapSpec, MountType, PortBind, SandboxSpec};

/// The operator's limits for remote callers.
#[derive(Debug, Clone, Default)]
pub struct RemotePolicy {
    pub allow_privileged: bool,
    /// raw_config, raw_devices, incus_profiles, idmap maps, guest-bound ports.
    pub allow_raw: bool,
    /// Host directories bind mounts may come from.
    pub bind_roots: Vec<PathBuf>,
    /// Host addresses a published port may listen on, besides loopback.
    pub publish_addresses: Vec<String>,
    /// Exec into, remove and read logs of any instance, not only the ones
    /// `isb serve` manages.
    pub any_instance: bool,
}

fn refuse(what: impl std::fmt::Display) -> Error {
    Error::invalid(format!(
        "refused for a remote caller: {what} (see `isb serve --help` for the flag that allows it)"
    ))
}

impl RemotePolicy {
    /// Check a whole compose file. `base` is where its relative paths resolve.
    pub fn check_file(&self, file: &ComposeFile, base: &Path) -> Result<()> {
        if file.incus_project.is_some() {
            return Err(refuse("incus_project"));
        }
        for (k, s) in &file.secrets {
            if s.file.is_some() {
                return Err(refuse(format!(
                    "secret {k:?} from a host file; pass its value in `secrets`"
                )));
            }
        }
        for (name, spec) in &file.services {
            self.check_spec(spec, base)
                .map_err(|e| Error::invalid(format!("service {name:?}: {e}")))?;
        }
        Ok(())
    }

    /// Check one sandbox spec.
    pub fn check_spec(&self, spec: &SandboxSpec, base: &Path) -> Result<()> {
        if spec.privileged == Some(true) && !self.allow_privileged {
            return Err(refuse("privileged"));
        }
        if !self.allow_raw {
            if !spec.raw_config.is_empty() {
                return Err(refuse("raw_config"));
            }
            if !spec.raw_devices.is_empty() {
                return Err(refuse("raw_devices"));
            }
            if spec.profiles.is_some() {
                return Err(refuse("incus_profiles"));
            }
            match &spec.idmap {
                None | Some(IdmapSpec::Mode(IdmapMode::Auto | IdmapMode::None)) => {}
                Some(_) => return Err(refuse("an idmap other than auto or none")),
            }
        }
        // isb's own labels tie an instance to a stack and its balancer: set
        // by hand, they would put a sandbox into another stack's rotation.
        let deploy_labels = spec.deploy.as_ref().map(|d| &d.labels);
        for k in spec
            .labels
            .keys()
            .chain(deploy_labels.into_iter().flat_map(|l| l.keys()))
        {
            if k.starts_with("isb.") {
                return Err(refuse(format!("label {k:?} (isb.* labels are isb's own)")));
            }
        }
        for v in &spec.volumes {
            if v.mount_type == MountType::Bind {
                self.check_bind(&v.source, base)?;
            }
        }
        for p in &spec.ports {
            if p.bind == PortBind::Guest {
                if !self.allow_raw {
                    return Err(refuse(
                        "a guest-bound port (bind: guest) reaching into the host",
                    ));
                }
                continue;
            }
            let listen =
                crate::plan::normalize_addr(&p.listen, "127.0.0.1").map_err(Error::invalid)?;
            if listen.starts_with("unix:") {
                return Err(refuse("a unix-socket listener on the host"));
            }
            let host = crate::plan::split_addr(&listen)
                .map(|(_, h, _)| h.to_string())
                .or_else(|| {
                    // A range: tcp:HOST:8000-8010.
                    listen.split(':').nth(1).map(String::from)
                })
                .unwrap_or_default();
            let h = host.trim_start_matches('[').trim_end_matches(']');
            let loopback = h
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
            if !loopback && !self.publish_addresses.iter().any(|a| a == h) {
                return Err(refuse(format!("publishing on {h}")));
            }
        }
        Ok(())
    }

    /// A bind source must lie inside a bind root once every symlink is
    /// followed.
    fn check_bind(&self, source: &str, base: &Path) -> Result<()> {
        if self.bind_roots.is_empty() {
            return Err(refuse(format!(
                "bind mount {source:?} (no --bind-root is set)"
            )));
        }
        let p = crate::plan::resolve_host_path(source, base)?;
        let real = std::fs::canonicalize(&p)
            .map_err(|e| Error::invalid(format!("bind mount {p}: {e}")))?;
        let inside = self
            .bind_roots
            .iter()
            .any(|r| std::fs::canonicalize(r).is_ok_and(|root| real.starts_with(&root)));
        if !inside {
            return Err(refuse(format!(
                "bind mount {} (outside every --bind-root)",
                real.display()
            )));
        }
        Ok(())
    }

    /// A base directory a remote caller asked for must be inside a bind root.
    pub fn check_base_dir(&self, dir: &Path) -> Result<()> {
        self.check_bind(&dir.to_string_lossy(), Path::new("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(y: &str) -> SandboxSpec {
        serde_yaml_ng::from_str(y).unwrap()
    }

    #[test]
    fn refuses_host_escapes() {
        let p = RemotePolicy::default();
        let base = Path::new("/");
        assert!(
            p.check_spec(&spec("image: x\nprivileged: true\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nraw_config: {a: b}\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nraw_devices: {d: {type: disk}}\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nincus_profiles: [default]\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nvolumes: ['/etc:/x']\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nports: ['0.0.0.0:80:80']\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(
                &spec(
                    "image: x\nports: [{listen: 'unix:/run/x.sock', connect: 'tcp:127.0.0.1:1'}]\n"
                ),
                base
            )
            .is_err()
        );
        assert!(
            p.check_spec(
                &spec("image: x\nports: [{listen: 'tcp:127.0.0.1:1', connect: 'unix:/var/lib/incus/unix.socket', bind: guest}]\n"),
                base
            )
            .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nidmap: {raw: 'both 0 0'}\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\nlabels: {isb.stack: app}\n"), base)
                .is_err()
        );
        assert!(
            p.check_spec(&spec("image: x\ndeploy: {labels: {isb.rev: x}}\n"), base)
                .is_err()
        );
        // Fine: loopback ports, named volumes, idmap auto.
        p.check_spec(
            &spec("image: x\nidmap: auto\nports: ['8080:80', '[::1]:81:81']\nvolumes: ['data:/data']\n"),
            base,
        )
        .unwrap();
    }

    #[test]
    fn publish_addresses_and_bind_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("app")).unwrap();
        std::os::unix::fs::symlink("/etc", dir.path().join("app/escape")).unwrap();
        let p = RemotePolicy {
            bind_roots: vec![dir.path().to_path_buf()],
            publish_addresses: vec!["100.86.22.100".into()],
            ..Default::default()
        };
        let base = dir.path();
        p.check_spec(
            &spec("image: x\nvolumes: ['./app:/app']\nports: ['100.86.22.100:80:80']\n"),
            base,
        )
        .unwrap();
        let e = p
            .check_spec(&spec("image: x\nvolumes: ['./app/escape:/x']\n"), base)
            .unwrap_err()
            .to_string();
        assert!(e.contains("outside"), "{e}");
        assert!(
            p.check_spec(&spec("image: x\nports: ['10.0.0.1:80:80']\n"), base)
                .is_err()
        );
    }

    #[test]
    fn file_level_checks() {
        let p = RemotePolicy::default();
        let f: ComposeFile = serde_yaml_ng::from_str(
            "secrets: {k: {file: /home/u/.ssh/id_ed25519}}\nservices: {}\n",
        )
        .unwrap();
        assert!(p.check_file(&f, Path::new("/")).is_err());
        let f: ComposeFile =
            serde_yaml_ng::from_str("incus_project: other\nservices: {}\n").unwrap();
        assert!(p.check_file(&f, Path::new("/")).is_err());
    }
}
