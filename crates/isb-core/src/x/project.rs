//! The project sandbox: one per project root, kept between runs (a create
//! costs tens of seconds, an exec a fraction of one), with the root mounted
//! at its own path and `.git` read-only.
//!
//! Its egress list only grows. It starts with a name that exists nowhere,
//! so the sandbox has DNS and a proxy that refuses (and records) every
//! connection; a package manager's first run adds its registry, once. A
//! list is never narrowed in place: changing it costs about ten seconds,
//! a run still going would lose hosts it relies on, and a guest that went
//! through `none` does not get its address back.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{CACHE_DIR, IMAGE, LABEL_KIND, LABEL_ROOT, PREFIX, USER};
use crate::client::Client;
use crate::egress::{EgressSecretSpec, EgressSpec, Policy};
use crate::error::{Error, Result};
use crate::sandbox::{EnsureOptions, Reporter, Sandbox};
use crate::spec::SandboxSpec;

/// What a run needs from its sandbox. Two runs that agree on all of it
/// share one sandbox.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    /// The host directory mounted at the same path (already checked).
    pub root: PathBuf,
    pub image: String,
    pub vm: bool,
    /// Mount `.git` writable too (by default it is read-only, so nothing
    /// inside can plant a hook the host's git would run).
    pub git_rw: bool,
    /// Mount the shared package cache volume.
    pub cache: bool,
    pub net: Net,
    /// The hosts and secrets this run needs, added to the sandbox's list.
    pub grant: EgressSpec,
}

/// The network of a project sandbox. Each is a sandbox of its own, since
/// one cannot move between them in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Net {
    /// The project's growing allow list.
    Project,
    /// No network at all.
    None,
    /// Any public name on 80 and 443 (`@public`), plus the grants.
    Public,
}

/// The entry a project sandbox's list starts with: a name that exists
/// nowhere, so the list is never empty (which would be `none`).
pub const CLOSED: &str = "closed.isbx.invalid";

impl RunSpec {
    pub fn new(root: PathBuf) -> RunSpec {
        RunSpec {
            root,
            image: IMAGE.into(),
            vm: false,
            git_rw: false,
            cache: true,
            net: Net::Project,
            grant: EgressSpec::default(),
        }
    }

    /// The egress list a new sandbox gets: [`CLOSED`] or `@public`, plus
    /// the grants; every name resolving to the proxy, so refusals are
    /// recorded.
    pub fn egress(&self) -> EgressSpec {
        let first = match self.net {
            Net::None => return EgressSpec::none(),
            Net::Project => CLOSED,
            Net::Public => "@public",
        };
        let mut allow = vec![first.to_string()];
        allow.extend(self.grant.allow.iter().cloned());
        EgressSpec {
            allow,
            none: false,
            secrets: self.grant.secrets.clone(),
            dns_all: true,
        }
    }

    /// `x-run-<hash>`: stable for the same root and settings, so a run finds
    /// the sandbox the last one left.
    pub fn instance(&self) -> String {
        let key = json!({
            "root": self.root,
            "image": self.image,
            "vm": self.vm,
            "git_rw": self.git_rw,
            "cache": self.cache,
            "net": self.net,
        });
        let d = ring::digest::digest(&ring::digest::SHA256, key.to_string().as_bytes());
        let hex: String = d.as_ref()[..5].iter().map(|b| format!("{b:02x}")).collect();
        format!("{PREFIX}run-{hex}")
    }

    /// The cache volume: one per trust zone. Containers share one; a VM,
    /// which is for code not trusted to share, gets its own.
    pub fn cache_volume(&self) -> String {
        if self.vm {
            format!("{}-cache", self.instance())
        } else {
            format!("{PREFIX}cache-ct")
        }
    }

    pub fn spec(&self) -> Result<SandboxSpec> {
        let root = self
            .root
            .to_str()
            .ok_or_else(|| Error::invalid(format!("{}: not a UTF-8 path", self.root.display())))?;
        let mut volumes = vec![format!("{root}:{root}")];
        if !self.git_rw && self.root.join(".git").is_dir() {
            volumes.push(format!("{root}/.git:{root}/.git:ro"));
        }
        let mut env = json!({
            "PATH": "/home/dev/.local/share/mise/shims:/home/dev/.local/bin:/home/dev/.cargo/bin:/usr/local/bin:/usr/bin:/bin",
            // The project's mise.toml is trusted inside the sandbox only.
            "MISE_TRUSTED_CONFIG_PATHS": root,
            "MISE_YES": "1",
        });
        if self.cache {
            volumes.push(format!("{}:{CACHE_DIR}:owner=dev", self.cache_volume()));
            for (k, sub) in [
                ("npm_config_cache", "npm"),
                ("PIP_CACHE_DIR", "pip"),
                ("UV_CACHE_DIR", "uv"),
                ("GOMODCACHE", "go"),
                ("BUN_INSTALL_CACHE_DIR", "bun"),
            ] {
                env[k] = json!(format!("{CACHE_DIR}/{sub}"));
            }
        }
        let v = json!({
            "container_name": self.instance(),
            "image": self.image,
            "type": if self.vm { "vm" } else { "container" },
            "idmap": "auto",
            "labels": { LABEL_KIND: "run", LABEL_ROOT: root },
            "volumes": volumes,
            "user": USER,
            "working_dir": root,
            "exec": { "env": env },
            "egress": self.egress(),
        });
        serde_json::from_value(v).map_err(|e| Error::invalid(format!("isbx spec: {e}")))
    }
}

/// The sandbox for `rs`, running: started if stopped, created if missing,
/// its list widened when the run needs more. The hot path (it exists, runs
/// and allows enough) is two state reads.
pub fn ensure(client: &Client, rs: &RunSpec, report: Reporter<'_>) -> Result<Sandbox> {
    let mut spec = rs.spec()?;
    let name = rs.instance();
    match Sandbox::get(client, &name) {
        Ok(sb) => {
            let sb = sb.with_exec_defaults(spec.exec_defaults());
            sb.start()?;
            if rs.net == Net::None {
                return Ok(sb);
            }
            let network = crate::egress::plumb::network_name(client.project_name(), &name);
            let have = crate::egress::stored_policy(&sb.info()?.config).unwrap_or_default();
            let Some(wider) = widen(&have, &rs.egress(), &network)? else {
                return Ok(sb);
            };
            report(&format!(
                "isbx: letting {name} also reach {} (kept for this project)",
                added(&have, &wider, &network)?.join(", ")
            ));
            spec.egress = Some(wider);
            let (sb, _) = Sandbox::connect_or_create_with_base(
                client,
                &spec,
                &Default::default(),
                Path::new("/"),
                EnsureOptions::default(),
                report,
            )?;
            wait_proxy(&network, &spec, report)?;
            Ok(sb)
        }
        Err(Error::NotFound(_)) => {
            report(&format!(
                "isbx: creating {name} for {} (once per project; later runs reuse it)",
                rs.root.display()
            ));
            let (sb, _) = Sandbox::connect_or_create_with_base(
                client,
                &spec,
                &Default::default(),
                Path::new("/"),
                EnsureOptions::default(),
                report,
            )?;
            if rs.net != Net::None {
                let network = crate::egress::plumb::network_name(client.project_name(), &name);
                wait_proxy(&network, &spec, report)?;
            }
            Ok(sb)
        }
        Err(e) => Err(e),
    }
}

/// How long a changed list may take to reach the proxy in `isb serve`.
const PROXY_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait until `isb serve`'s proxy applies the list in `spec`, so the first
/// connection of the command is not refused by the list it replaced.
fn wait_proxy(network: &str, spec: &SandboxSpec, report: Reporter<'_>) -> Result<()> {
    let egress = spec.egress.clone().unwrap_or_default();
    let policy = Policy::from_spec(&egress, network)?;
    if !crate::egress::names::wait_applied(network, &policy, PROXY_WAIT) {
        report(&format!(
            "isbx: isb serve's egress proxy has not confirmed the new list after {}s; \
the first connections may be refused (is `isb serve` running, and as new as this isb?)",
            PROXY_WAIT.as_secs()
        ));
    }
    Ok(())
}

/// The list `have` grown by what `want` adds, or `None` when it already
/// covers it. Secrets are matched by variable; a changed one replaces the old.
pub fn widen(have: &Policy, want: &EgressSpec, network: &str) -> Result<Option<EgressSpec>> {
    let w = Policy::from_spec(want, network)?;
    let new_entry = w.entries.iter().any(|e| !have.entries.contains(e));
    let new_secret = w.secrets.iter().any(|s| {
        !have
            .secrets
            .iter()
            .any(|h| h.env == s.env && h.secret == s.secret && h.hosts == s.hosts)
    });
    if !new_entry && !new_secret {
        return Ok(None);
    }
    let mut allow = have.list();
    for e in w.list() {
        if !allow.contains(&e) {
            allow.push(e);
        }
    }
    let mut secrets: Vec<EgressSecretSpec> = have
        .secrets
        .iter()
        .filter(|h| !w.secrets.iter().any(|s| s.env == h.env))
        .chain(&w.secrets)
        .map(|b| EgressSecretSpec {
            env: b.env.clone(),
            secret: Some(b.secret.clone()),
            hosts: b.hosts.iter().map(ToString::to_string).collect(),
        })
        .collect();
    secrets.sort_by(|a, b| a.env.cmp(&b.env));
    Ok(Some(EgressSpec {
        allow,
        none: false,
        secrets,
        dns_all: true,
    }))
}

/// The `host:port` entries `wider` has and `have` lacks, for the report.
fn added(have: &Policy, wider: &EgressSpec, network: &str) -> Result<Vec<String>> {
    let w = Policy::from_spec(wider, network)?;
    let mut out: Vec<String> = w
        .entries
        .iter()
        .filter(|e| !have.entries.contains(e))
        .map(ToString::to_string)
        .collect();
    out.extend(
        w.secrets
            .iter()
            .filter(|s| !have.secrets.iter().any(|h| h.env == s.env))
            .map(|s| format!("${} on the wire", s.env)),
    );
    Ok(out)
}

/// A sandbox for `rs` of its own, for one run (`--fresh`), which the caller
/// removes afterwards. It shares nothing with the kept one but the cache.
pub fn create_fresh(client: &Client, rs: &RunSpec, report: Reporter<'_>) -> Result<Sandbox> {
    let mut spec = rs.spec()?;
    let name = format!("{}-{:x}", rs.instance(), std::process::id());
    report(&format!("isbx: creating {name} for this run only"));
    spec.name = Some(name);
    Sandbox::create_with(
        client,
        &spec,
        &Default::default(),
        EnsureOptions::default(),
        report,
    )
}

/// The working directory inside: the caller's own when it is under the
/// mounted root (it is the same path inside), else the root.
pub fn guest_cwd(root: &Path, cwd: &Path) -> PathBuf {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if cwd.starts_with(root) {
        cwd
    } else {
        root.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_stable_and_changes_with_any_setting() {
        let a = RunSpec::new("/home/u/app".into());
        assert_eq!(a.instance(), RunSpec::new("/home/u/app".into()).instance());
        assert!(a.instance().starts_with("x-run-") && a.instance().len() == 16);
        // What a run is granted does not change which sandbox it uses.
        let mut b = a.clone();
        b.grant = EgressSpec::allow(["@npm"]);
        assert_eq!(a.instance(), b.instance());
        b.net = Net::None;
        assert_ne!(a.instance(), b.instance());
        let mut c = a.clone();
        c.vm = true;
        assert_ne!(a.instance(), c.instance());
        assert_ne!(a.cache_volume(), c.cache_volume());
        assert_ne!(
            a.instance(),
            RunSpec::new("/home/u/other".into()).instance()
        );
    }

    #[test]
    fn the_spec_mounts_the_root_at_its_path_and_git_read_only() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        let mut rs = RunSpec::new(root.clone());
        rs.grant = EgressSpec::allow(["@npm"]);
        let s = rs.spec().unwrap();
        let y = serde_yaml_ng::to_string(&s).unwrap();
        let r = root.display();
        assert!(y.contains(&format!("source: {r}\n  target: {r}\n")), "{y}");
        assert!(
            y.contains(&format!(
                "source: {r}/.git\n  target: {r}/.git\n  read_only: true"
            )),
            "{y}"
        );
        assert!(
            y.contains("source: x-cache-ct\n  target: /var/cache/isbx"),
            "{y}"
        );
        assert_eq!(s.user.as_deref(), Some(USER));
        assert_eq!(s.labels.get(LABEL_KIND).map(String::as_str), Some("run"));
        let e = s.egress.unwrap();
        assert_eq!(e.allow, [CLOSED, "@npm"]);
        assert!(e.dns_all && !e.none);
        // --git rw, no network, no cache.
        rs.git_rw = true;
        rs.cache = false;
        rs.net = Net::None;
        let s = rs.spec().unwrap();
        let y = serde_yaml_ng::to_string(&s).unwrap();
        assert!(!y.contains("/.git") && !y.contains("x-cache"), "{y}");
        assert!(s.egress.is_some_and(|e| e.none));
    }

    #[test]
    fn the_list_only_grows_and_only_when_something_is_new() {
        let n = "isbbrxtest";
        let mut rs = RunSpec::new("/p".into());
        rs.grant = EgressSpec::allow(["@npm"]);
        let have = Policy::from_spec(&rs.egress(), n).unwrap();
        // The same grant, or none: nothing to do.
        assert!(widen(&have, &rs.egress(), n).unwrap().is_none());
        rs.grant = EgressSpec::default();
        assert!(widen(&have, &rs.egress(), n).unwrap().is_none());
        // A new host: the old list plus it, the old hosts kept.
        rs.grant = EgressSpec {
            allow: vec!["api.example.com".into()],
            secrets: vec![EgressSecretSpec::parse("K@api.example.com").unwrap()],
            ..Default::default()
        };
        let w = widen(&have, &rs.egress(), n).unwrap().unwrap();
        let p = Policy::from_spec(&w, n).unwrap();
        assert!(p.allows("registry.npmjs.org", 443) && p.allows("api.example.com", 443));
        assert!(p.allows(CLOSED, 443) && p.dns_all);
        assert_eq!(p.secrets.len(), 1);
        assert_eq!(
            added(&have, &w, n).unwrap(),
            ["api.example.com:443", "$K on the wire"]
        );
        assert!(widen(&p, &rs.egress(), n).unwrap().is_none());
    }

    #[test]
    fn the_cwd_inside_is_the_callers_when_under_the_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        assert_eq!(guest_cwd(&root, &root.join("sub")), root.join("sub"));
        assert_eq!(guest_cwd(&root, Path::new("/")), root);
    }
}
