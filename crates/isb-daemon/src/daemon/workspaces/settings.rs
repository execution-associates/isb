//! `workspace_settings`: an org's workspace limits and defaults.

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    max_workspaces: Option<u32>,
    #[serde(default)]
    sandbox_expiry: Option<String>,
    #[serde(default)]
    sandbox_idle: Option<String>,
    /// `""` clears it.
    #[serde(default)]
    home_pool: Option<String>,
    /// `volume`, `host`, or `""` (the daemon's default).
    #[serde(default)]
    home_kind: Option<String>,
}

pub(super) fn workspace_settings(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: SettingsArgs = args(a)?;
    let wsm = &d.workspaces;
    let mut s = wsm.store.settings(&org)?;
    let changes = a.max_workspaces.is_some()
        || a.sandbox_expiry.is_some()
        || a.sandbox_idle.is_some()
        || a.home_pool.is_some()
        || a.home_kind.is_some();
    if changes {
        let pool_change = a
            .home_pool
            .as_ref()
            .is_some_and(|p| Some(p.as_str()).filter(|p| !p.is_empty()) != s.home_pool.as_deref());
        let kind_change = a
            .home_kind
            .as_ref()
            .is_some_and(|k| Some(k.as_str()).filter(|k| !k.is_empty()) != s.home_kind.as_deref());
        if a.max_workspaces.is_some_and(|m| m != s.max_workspaces) || pool_change || kind_change {
            let platform = match c {
                Caller::Local { .. } | Caller::Superadmin(_) => true,
                Caller::User { principal } => principal.platform_admin,
                _ => false,
            };
            if !platform {
                return Err(Error::Forbidden(
                    "max_workspaces, home_pool and home_kind are for platform admins".into(),
                ));
            }
        }
        require(c, &org, Role::Admin, "changing workspace settings")?;
        if let Some(m) = a.max_workspaces {
            s.max_workspaces = m;
        }
        if let Some(e) = a.sandbox_expiry {
            s.sandbox_expiry = e.trim().to_string();
        }
        if let Some(i) = a.sandbox_idle {
            s.sandbox_idle = i.trim().to_string();
        }
        if let Some(k) = a.home_kind {
            s.home_kind = match k.trim() {
                "" => None,
                "volume" | "host" => Some(k.trim().to_string()),
                other => {
                    return Err(Error::invalid(format!(
                        "home_kind {other:?}: volume, host, or \"\" for the daemon's default"
                    )));
                }
            };
        }
        if let Some(p) = a.home_pool {
            let p = p.trim().to_string();
            if p.is_empty() {
                s.home_pool = None;
            } else {
                pool_driver(&d.client, &p)
                    .map_err(|e| Error::invalid(format!("home_pool {p}: {e}")))?;
                s.home_pool = Some(p);
            }
        }
        wsm.store.put_settings(&org, &s)?;
    }
    Ok(json!({"org": org.as_str(), "settings": s}))
}
