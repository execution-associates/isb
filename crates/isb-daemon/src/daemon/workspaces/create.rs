//! `workspace_create`: check the call, place the home, build the machine,
//! and when the build fails take away everything the call made (the
//! instance, the definition, the token, and a home volume or folder it
//! created), so a retry starts clean.

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    cpus: Option<u32>,
    #[serde(default)]
    memory: Option<String>,
    #[serde(default)]
    root_size: Option<String>,
    #[serde(default)]
    home_size: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    token_role: Option<Role>,
    #[serde(default)]
    home_bind: Option<String>,
}

pub(super) fn check_size(what: &str, s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 20
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{what} {s:?}: a size such as 20GiB or 512MiB"
        )))
    }
}

pub(super) fn check_fields(
    env: &BTreeMap<String, String>,
    secrets: &[String],
    labels: &BTreeMap<String, String>,
) -> Result<()> {
    for k in env.keys() {
        ws::check_env_name(k)?;
    }
    for s in secrets {
        if s.is_empty() || s.contains('/') || s.starts_with('.') || s.len() > 128 {
            return Err(Error::invalid(format!("secret name {s:?}")));
        }
    }
    for k in labels.keys() {
        if k.starts_with("isb.") || k.is_empty() || k.contains(char::is_whitespace) {
            return Err(Error::invalid(format!(
                "label {k:?}: isb.* labels are isb's own"
            )));
        }
    }
    Ok(())
}

pub(super) fn token_role(r: Option<Role>) -> Result<Role> {
    match r.unwrap_or(Role::Admin) {
        Role::Owner => Err(Error::invalid(
            "token_role: viewer, member or admin (an owner's reach is for people)",
        )),
        r => Ok(r),
    }
}

/// The call's own checks, before anything is looked up: the workspace it
/// describes, without its home placed or its token minted.
fn checked(a: CreateArgs, c: &Caller) -> Result<Workspace> {
    let name = a.name.clone().unwrap_or_else(|| ws::DEFAULT_NAME.into());
    ws::check_name(&name)?;
    let user = a.user.clone().unwrap_or_else(|| ws::DEFAULT_USER.into());
    ws::check_user(&user)?;
    let home_size = a
        .home_size
        .clone()
        .unwrap_or_else(|| ws::DEFAULT_HOME_SIZE.into());
    check_size("home_size", &home_size)?;
    if let Some(r) = &a.root_size {
        check_size("root_size", r)?;
    }
    check_fields(&a.env, &a.secrets, &a.labels)?;
    if a.home_bind.is_some() && !c.is_trusted() {
        return Err(Error::Forbidden(
            "home_bind (a host directory as the home) is for superadmins".into(),
        ));
    }
    if a.home_bind.as_ref().is_some_and(|b| !b.starts_with('/')) {
        return Err(Error::invalid("home_bind must be an absolute host path"));
    }
    let t = now();
    Ok(Workspace {
        name,
        id: random_hex(8),
        image: a.image.as_deref().unwrap_or_default().trim().to_string(),
        user,
        cpus: a.cpus,
        memory: a.memory,
        root_size: a.root_size,
        home_size,
        env: a.env,
        secrets: a.secrets,
        labels: a.labels,
        token_role: token_role(a.token_role)?,
        home_bind: a.home_bind,
        pool: None,
        created_at: t,
        created_by: creator(c),
        updated_at: t,
        rebuilt_at: None,
        token: None,
        ports: vec![],
    })
}

/// Whether the org has room for workspace `w`: its name free among the
/// workspaces and the instances, the org under `max_workspaces`, and its
/// secrets readable.
fn check_room(d: &Daemon, org: &OrgId, w: &Workspace, settings: &Settings) -> Result<()> {
    let wsm = &d.workspaces;
    let name = &w.name;
    let existing = wsm.store.list(org)?;
    if existing.iter().any(|x| x.name == *name) {
        return Err(Error::AlreadyExists(format!(
            "org {org} already has workspace {name}"
        )));
    }
    if existing.len() as u32 >= settings.max_workspaces {
        return Err(Error::invalid(format!(
            "org {org} has its workspace ({}); an org has {} (a platform admin can raise max_workspaces with workspace_settings)",
            existing
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            settings.max_workspaces
        )));
    }
    if Sandbox::get(&wsm.oc(org), name).is_ok() {
        return Err(Error::AlreadyExists(format!(
            "an instance named {name} exists in org {org}; pick another workspace name"
        )));
    }
    for s in &w.secrets {
        d.secrets
            .inspect(org, s)
            .map_err(|e| Error::invalid(format!("secret {s}: {e}")))?;
    }
    Ok(())
}

/// Where the home goes: the path a superadmin named, else a host folder
/// under --workspace-home-root (unless the org says volume), else a
/// managed volume in the org's home pool.
fn place_home(wsm: &Workspaces, org: &OrgId, settings: &Settings, w: &mut Workspace) -> Result<()> {
    if w.home_bind.is_none() {
        match (&wsm.home_root, settings.home_kind.as_deref()) {
            (Some(root), None | Some("host")) => {
                w.home_bind = Some(ws::host_home(root, org, &w.name).display().to_string());
            }
            (None, Some("host")) => {
                return Err(Error::invalid(
                    "org setting home_kind is host, but isb serve has no --workspace-home-root",
                ));
            }
            _ => {}
        }
    }
    if w.home_bind.is_none() {
        w.pool = Some(wsm.new_home_pool(org, &wsm.oc(org))?);
    }
    Ok(())
}

/// The home as it was before the build: whether it already existed, so a
/// failed create removes only a home it made.
struct HomeBefore {
    existed: bool,
}

fn home_before(wsm: &Workspaces, org: &OrgId, w: &Workspace) -> Result<HomeBefore> {
    let existed = match (&w.home_bind, &w.pool) {
        (Some(dir), _) => Path::new(dir).exists(),
        (None, Some(pool)) => {
            crate::volume::get(&wsm.oc(org), pool, &w.home_volume(org))?.is_some()
        }
        (None, None) => true,
    };
    Ok(HomeBefore { existed })
}

/// Undo a failed create: the instance, the definition and the token, and
/// the home when this call made it.
fn undo(wsm: &Workspaces, org: &OrgId, w: &Workspace, before: &HomeBefore) -> Result<()> {
    let oc = wsm.oc(org);
    let _ = Sandbox::remove(&oc, &w.name, true);
    wsm.store.delete(org, &w.name)?;
    wsm.index(
        org,
        &Workspace {
            token: None,
            ..w.clone()
        },
    );
    if before.existed {
        return Ok(());
    }
    match (&w.home_bind, &w.pool) {
        (Some(dir), _) => {
            let _ = std::fs::remove_dir_all(dir);
        }
        (None, Some(pool)) => {
            let _ = crate::volume::remove(&oc, pool, &w.home_volume(org));
        }
        (None, None) => {}
    }
    Ok(())
}

/// On a copy-on-write pool the home gets snapshots from the start, hourly
/// with the last day kept. On `dir` and the like every snapshot is a full
/// copy of the home, so none are scheduled: backups to S3 instead, or an
/// admin opts in with a small keep.
fn schedule_home_snapshots(d: &Daemon, org: &OrgId, w: &Workspace, log: &mut Vec<String>) {
    let (None, Some(pool)) = (&w.home_bind, &w.pool) else {
        return;
    };
    match pool_driver(&d.client, pool) {
        Ok(drv) if copy_on_write(&drv) => {
            match d.volumes.update(
                org,
                &w.home_volume(org),
                &json!({"schedule": HOME_SNAPSHOTS, "keep": HOME_SNAPSHOTS_KEEP}),
            ) {
                Ok(_) => log.push(format!(
                    "home snapshots {HOME_SNAPSHOTS}, keeping {HOME_SNAPSHOTS_KEEP} (pool {pool}, {drv})"
                )),
                Err(e) => log.push(format!("home snapshot schedule not set: {e}")),
            }
        }
        Ok(drv) => log.push(format!(
            "no automatic home snapshots: pool {pool} is {drv}, where each snapshot is a full copy of the home"
        )),
        Err(e) => log.push(format!("pool {pool}: {e}")),
    }
}

pub(super) fn workspace_create(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let mut a: CreateArgs = args(a)?;
    require(c, &org, Role::Admin, "creating a workspace")?;
    if a.image.as_deref().is_none_or(|i| i.trim().is_empty()) {
        a.image = Some(images::default_for(&d.workspaces.oc(&org)));
    }
    let mut w = checked(a, c)?;
    let wsm = d.workspaces.clone();
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let settings = wsm.store.settings(&org)?;
    check_room(d, &org, &w, &settings)?;
    place_home(&wsm, &org, &settings, &mut w)?;
    if w.root_size.is_none() && project_has_disk_limit(&d.client, &org) {
        // incus needs a root size in an org with a disk quota.
        w.root_size = Some(DEFAULT_ROOT_SIZE.into());
    }
    let before = home_before(&wsm, &org, &w)?;
    wsm.mint(&org, &mut w)?;
    wsm.store.put(&org, &w)?;
    let mut log = Vec::new();
    if let Err(e) = wsm.build(&org, &w, &mut log) {
        undo(&wsm, &org, &w, &before)?;
        return Err(e);
    }
    schedule_home_snapshots(d, &org, &w, &mut log);
    wsm.ensure_bridge(&org);
    // The bridge may have come up after the first delivery: write the
    // profile again with its URL.
    let _ = wsm.deliver(&org, &w);
    let (name, role) = (&w.name, w.token_role);
    wsm.record(
        &org,
        "workspace.created",
        name,
        &creator(c),
        format!("workspace {name} created from {}", w.image),
        json!({"image": w.image, "token_role": role}),
    );
    let mut v = view(d, &org, &w, false);
    v["log"] = json!(log);
    v["message"] = json!(format!(
        "Workspace {name} is running. Its token (role {}) is at {} inside it and in $ISB_TOKEN for login shells; it is never shown here.",
        role.as_str(),
        ws::TOKEN_PATH
    ));
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{a_workspace, manager};
    use super::*;

    /// What a create makes before its build: the home folder, the token,
    /// the definition.
    fn made(m: &Workspaces, org: &OrgId, w: &mut Workspace) -> HomeBefore {
        let before = home_before(m, org, w).unwrap();
        std::fs::create_dir_all(w.home_bind.as_ref().unwrap()).unwrap();
        m.mint(org, w).unwrap();
        m.store.put(org, w).unwrap();
        before
    }

    #[test]
    fn a_failed_create_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("homes");
        let m = manager(d.path(), Some(root.clone()));
        let org = OrgId::new("lab").unwrap();
        let mut w = a_workspace();
        place_home(&m, &org, &Settings::default(), &mut w).unwrap();
        let home = PathBuf::from(w.home_bind.clone().unwrap());
        assert!(home.starts_with(root.join("lab")));
        let before = made(&m, &org, &mut w);
        let token = m.token_plain(&org, &w.name).unwrap().unwrap();
        let token = String::from_utf8(token).unwrap();
        assert!(m.authenticate(&token).is_some());

        undo(&m, &org, &w, &before).unwrap();
        assert!(m.store.get(&org, &w.name).unwrap().is_none());
        assert!(m.token_plain(&org, &w.name).unwrap().is_none());
        assert!(m.authenticate(&token).is_none());
        assert!(!home.exists(), "the home folder this create made");
    }

    #[test]
    fn a_failed_create_keeps_a_home_it_did_not_make() {
        let d = tempfile::tempdir().unwrap();
        let m = manager(d.path(), None);
        let org = OrgId::new("lab").unwrap();
        let home = d.path().join("mine");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("notes"), "keep").unwrap();
        let mut w = Workspace {
            home_bind: Some(home.display().to_string()),
            ..a_workspace()
        };
        let before = made(&m, &org, &mut w);
        undo(&m, &org, &w, &before).unwrap();
        assert!(m.store.get(&org, &w.name).unwrap().is_none());
        assert!(home.join("notes").exists());
    }
}
