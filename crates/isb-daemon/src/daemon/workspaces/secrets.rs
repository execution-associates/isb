//! What a running workspace is given: its token, its named secrets and its
//! login environment, and a new secret value as it changes.

use super::Workspaces;
use crate::client::encode_segment;
use crate::error::Result;
use crate::org::OrgId;
use crate::workspace::{self as ws, Workspace};

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
                    Ok((v, _)) => oc.push_file(
                        inst,
                        &format!("{}/{name}", ws::SECRETS_DIR),
                        &v,
                        u.uid,
                        u.gid,
                        0o400,
                    )?,
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

    /// A secret got a new value: write it into every running workspace of
    /// the org that takes it (`/run/isb/secrets/NAME`), and restart none of
    /// them, since a workspace restarts only with `confirm`: what already
    /// read the old value keeps it. Returns `(workspace, what happened)`.
    pub fn secret_changed(&self, org: &OrgId, name: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for w in self.store.list(org).unwrap_or_default() {
            if !w.secrets.iter().any(|s| s == name) {
                continue;
            }
            let path = format!("{}/{name}", ws::SECRETS_DIR);
            let r = (|| -> Result<bool> {
                let oc = self.oc(org);
                let pid = oc
                    .get_opt(&format!(
                        "/1.0/instances/{}/state",
                        encode_segment(w.instance())
                    ))?
                    .and_then(|s| s["pid"].as_i64())
                    .unwrap_or(0);
                if pid <= 0 {
                    return Ok(false);
                }
                let (v, _) = self.secrets.get(org, name)?;
                let u = crate::exec::resolve_user(&oc, w.instance(), &w.user)?;
                oc.make_dir(w.instance(), ws::SECRETS_DIR, u.uid, u.gid, 0o700)?;
                oc.push_file(w.instance(), &path, &v, u.uid, u.gid, 0o400)?;
                Ok(true)
            })();
            let what = match r {
                Ok(true) => format!(
                    "delivered {path}; not restarted (a workspace restarts only with confirm): processes that read the old value keep it"
                ),
                Ok(false) => "not running: gets the new value when it starts".to_string(),
                Err(e) => format!("not delivered: {e}; it gets the value when it next starts"),
            };
            out.push((w.name.clone(), what));
        }
        out
    }
}
