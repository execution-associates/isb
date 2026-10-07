//! What a running workspace is given: its token, its named secrets and its
//! login environment, and a new secret value as it changes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::Workspaces;
use crate::client::encode_segment;
use crate::error::Result;
use crate::org::OrgId;
use crate::workspace::{self as ws, Workspace};

/// What the workspaces' secret poller remembers: when each org is next due,
/// and the version of each secret last written into each workspace.
#[derive(Default)]
pub struct Poll {
    next: HashMap<OrgId, Instant>,
    /// (org, workspace, secret) to version.
    versions: HashMap<(String, String, String), u64>,
}

impl Workspaces {
    /// Write the token, the named secrets and the login environment into a
    /// running workspace. Files only, through incus' file API.
    pub(super) fn deliver(&self, org: &OrgId, w: &Workspace) -> Result<()> {
        let oc = self.oc(org);
        let Some(state) = oc.get_opt(&format!(
            "/1.0/instances/{}/state",
            encode_segment(w.instance())
        ))?
        else {
            return Ok(());
        };
        let pid = state["pid"].as_i64().unwrap_or(0);
        if pid <= 0 {
            return Ok(());
        }
        let u = crate::exec::resolve_user(&oc, w.instance(), &w.user)?;
        let inst = w.instance();
        oc.make_dir(inst, "/run/isb", 0, 0, 0o755)?;
        if let Some(t) = self.token_plain(org, &w.name)? {
            oc.push_file(inst, ws::TOKEN_PATH, &t, u.uid, u.gid, 0o400)?;
        }
        if !w.secrets.is_empty() {
            oc.make_dir(inst, ws::SECRETS_DIR, u.uid, u.gid, 0o700)?;
            for name in &w.secrets {
                match self.secrets.get(org, name) {
                    Ok((v, m)) => {
                        oc.push_file(
                            inst,
                            &format!("{}/{name}", ws::SECRETS_DIR),
                            &v,
                            u.uid,
                            u.gid,
                            0o400,
                        )?;
                        self.saw(org, &w.name, name, m.version);
                    }
                    Err(e) => eprintln!(
                        "isb serve: workspace {org}/{}: secret {name} not delivered: {e}",
                        w.name
                    ),
                }
            }
        }
        oc.make_dir(inst, "/etc/profile.d", 0, 0, 0o755)?;
        let profile = ws::profile(self.url(org).as_deref(), org, w);
        oc.push_file(inst, ws::PROFILE_PATH, profile.as_bytes(), 0, 0, 0o644)?;
        self.delivered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(super::key(&org.incus_project(), inst), pid);
        Ok(())
    }

    /// Remember the version of `name` last written into a workspace.
    fn saw(&self, org: &OrgId, w: &str, name: &str, version: u64) {
        let k = (org.to_string(), w.to_string(), name.to_string());
        let mut p = self.secret_poll.lock().unwrap_or_else(|e| e.into_inner());
        p.versions.insert(k, version);
    }

    /// Write the current value of `name` into a workspace if it runs:
    /// `Some(version)` when written, `None` when it is not running.
    fn push_secret(&self, org: &OrgId, w: &Workspace, name: &str) -> Result<Option<u64>> {
        let oc = self.oc(org);
        let pid = oc
            .get_opt(&format!(
                "/1.0/instances/{}/state",
                encode_segment(w.instance())
            ))?
            .and_then(|s| s["pid"].as_i64())
            .unwrap_or(0);
        if pid <= 0 {
            return Ok(None);
        }
        let (v, m) = self.secrets.get(org, name)?;
        let u = crate::exec::resolve_user(&oc, w.instance(), &w.user)?;
        oc.make_dir(w.instance(), ws::SECRETS_DIR, u.uid, u.gid, 0o700)?;
        let path = format!("{}/{name}", ws::SECRETS_DIR);
        oc.push_file(w.instance(), &path, &v, u.uid, u.gid, 0o400)?;
        self.saw(org, &w.name, name, m.version);
        Ok(Some(m.version))
    }

    /// Whether any workspace of the org takes `name`.
    pub fn uses(&self, org: &OrgId, name: &str) -> bool {
        let list = self.store.list(org).unwrap_or_default();
        list.iter().any(|w| w.secrets.iter().any(|s| s == name))
    }

    /// A secret got a new value: write it into every running workspace of
    /// the org that takes it (`/run/isb/secrets/NAME`), and restart none of
    /// them, since a workspace restarts only with `confirm`: what already
    /// read the old value keeps it. Returns `(workspace, what happened)`.
    pub fn secret_changed(&self, org: &OrgId, name: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for w in self.store.list(org).unwrap_or_default() {
            if w.secrets.iter().any(|s| s == name) {
                out.push((
                    w.name.clone(),
                    outcome(name, self.push_secret(org, &w, name)),
                ));
            }
        }
        out
    }

    /// Every org's `secret_refresh`, check the driver references (names
    /// with a `/`) its workspaces take, in one round per org, and write a
    /// new version into each running workspace that takes it, with a
    /// `secret.rotated` event (stack `<org>/@workspaces`). Store secrets
    /// need no polling: `isb secret set` delivers them.
    pub(super) fn poll_secrets(&self, ctl: &crate::stack::Controller) {
        let now = Instant::now();
        for org in self.store.orgs() {
            let every = self
                .store
                .settings(&org)
                .ok()
                .and_then(|s| ws::secret_refresh(&s.secret_refresh).ok())
                .unwrap_or(Duration::from_secs(3600));
            {
                let mut p = self.secret_poll.lock().unwrap_or_else(|e| e.into_inner());
                // First sight: the boot delivery just wrote what is current.
                let next = p.next.entry(org.clone()).or_insert(now + every);
                if now < *next {
                    continue;
                }
                *next = now + every;
            }
            self.poll_org(ctl, &org);
        }
    }

    fn poll_org(&self, ctl: &crate::stack::Controller, org: &OrgId) {
        let list = self.store.list(org).unwrap_or_default();
        let mut names: Vec<&str> = list
            .iter()
            .flat_map(|w| w.secrets.iter().map(String::as_str))
            .filter(|n| n.contains('/'))
            .collect();
        names.sort_unstable();
        names.dedup();
        if names.is_empty() {
            return;
        }
        let current: HashMap<&str, u64> = names
            .iter()
            .zip(self.secrets.versions(org, &names))
            .filter_map(|(n, r)| match r {
                Ok(v) => Some((*n, v)),
                Err(e) => {
                    eprintln!(
                        "isb serve: workspaces {org}: secret {n}: cannot check its version: {e}"
                    );
                    None
                }
            })
            .collect();
        let stack = crate::stack::qualified(org, "@workspaces");
        let moved = {
            let mut p = self.secret_poll.lock().unwrap_or_else(|e| e.into_inner());
            moved(org, &list, &current, &mut p.versions)
        };
        for (w, name, from, to) in moved {
            let r = self.push_secret(org, w, &name);
            if matches!(r, Ok(None)) {
                // Stopped: its start delivers it; nothing to say twice.
                self.saw(org, &w.name, &name, to);
                continue;
            }
            ctl.event(
                "secret.rotated",
                "warn",
                &stack,
                &w.name,
                format!(
                    "new version of secret {name} (v{from} -> v{to}): workspace {} {}",
                    w.name,
                    outcome(&name, r)
                ),
            );
        }
    }
}

/// The driver references whose `current` version differs from the one last
/// written into a workspace: `(workspace, name, from, to)`. A reference
/// never written there (a stopped workspace) is recorded at `current`, not
/// reported: its next start delivers it.
fn moved<'a>(
    org: &OrgId,
    list: &'a [Workspace],
    current: &HashMap<&str, u64>,
    seen: &mut HashMap<(String, String, String), u64>,
) -> Vec<(&'a Workspace, String, u64, u64)> {
    let mut out = Vec::new();
    for w in list {
        for name in w.secrets.iter().filter(|n| n.contains('/')) {
            let Some(&to) = current.get(name.as_str()) else {
                continue;
            };
            let k = (org.to_string(), w.name.clone(), name.clone());
            match seen.get(&k) {
                Some(&from) if from != to => out.push((w, name.clone(), from, to)),
                Some(_) => {}
                None => {
                    seen.insert(k, to);
                }
            }
        }
    }
    out
}

/// What writing a secret into a workspace came to, for reports.
fn outcome(name: &str, r: Result<Option<u64>>) -> String {
    let path = format!("{}/{name}", ws::SECRETS_DIR);
    match r {
        Ok(Some(_)) => format!(
            "delivered {path}; not restarted (a workspace restarts only with confirm): processes that read the old value keep it"
        ),
        Ok(None) => "not running: gets the new value when it starts".to_string(),
        Err(e) => format!("not delivered: {e}; it gets the value when it next starts"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, secrets: &[&str]) -> Workspace {
        let mut w: Workspace = serde_json::from_value(serde_json::json!({
            "name": name, "id": "x", "image": "dev-base", "user": "dev",
            "home_size": "1GiB", "token_role": "admin", "created_at": 0, "created_by": "t",
        }))
        .unwrap();
        w.secrets = secrets.iter().map(|s| s.to_string()).collect();
        w
    }

    #[test]
    fn only_moved_references_are_reported() {
        let org = OrgId::default_org();
        let list = [
            ws("a", &["ops/db/pw", "local-name"]),
            ws("b", &["ops/db/pw", "ops/x/y"]),
        ];
        let mut seen = HashMap::new();
        let cur = HashMap::from([("ops/db/pw", 3), ("ops/x/y", 1), ("local-name", 9)]);
        // First sight records; nothing is reported, store names never are.
        assert!(moved(&org, &list, &cur, &mut seen).is_empty());
        assert_eq!(seen.len(), 3);
        let cur = HashMap::from([("ops/db/pw", 4), ("ops/x/y", 1)]);
        let m = moved(&org, &list, &cur, &mut seen);
        let got: Vec<(&str, &str, u64, u64)> = m
            .iter()
            .map(|(w, n, f, t)| (w.name.as_str(), n.as_str(), *f, *t))
            .collect();
        assert_eq!(got, [("a", "ops/db/pw", 3, 4), ("b", "ops/db/pw", 3, 4)]);
        // A version that could not be read is left alone.
        assert!(moved(&org, &list, &HashMap::new(), &mut seen).is_empty());
    }
}
