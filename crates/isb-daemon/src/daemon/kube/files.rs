//! `instance_file_read` and `instance_file_write`: small files in and out of
//! an instance, never isb's own.

use super::*;

/// The largest file `instance_file_read` returns.
pub(super) const FILE_READ_CAP: usize = 4 * 1024 * 1024;
/// The largest file `instance_file_write` takes: base64 of it, in a request,
/// must fit the HTTP body limit (4 MiB).
pub(super) const FILE_WRITE_CAP: usize = 2 * 1024 * 1024;

/// Where isb keeps what it delivers into instances: the workspace token,
/// stack and app secrets, the supervised service's environment and the
/// egress CA. Writes there are refused.
const MANAGED_DIRS: &[&str] = &["/run/isb", "/run/secrets", "/etc/isb"];
/// Kernel and device trees: neither read nor written through the file tools.
const VIRTUAL_DIRS: &[&str] = &["/proc", "/sys", "/dev"];

/// An absolute path with `.` and empty parts resolved; `..` and NUL refused.
pub(super) fn clean_path(p: &str) -> Result<String> {
    if !p.starts_with('/') {
        return Err(Error::invalid(format!("path {p:?} must be absolute")));
    }
    if p.contains('\0') || p.len() > 4096 {
        return Err(Error::invalid("path: no NUL, at most 4096 characters"));
    }
    let mut parts = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(Error::invalid(format!("path {p:?} must not contain .."))),
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return Err(Error::invalid("path names the root directory, not a file"));
    }
    Ok(format!("/{}", parts.join("/")))
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Why a file may not be read or written through the file tools, if so.
/// `managed` are the files isb itself delivers into this instance.
pub(super) fn refuse_path(path: &str, write: bool, managed: &[String]) -> Option<String> {
    if VIRTUAL_DIRS.iter().any(|d| under(path, d)) {
        return Some(format!(
            "{path} is a kernel or device file, not a regular one"
        ));
    }
    if path == crate::workspace::TOKEN_PATH {
        return Some(format!(
            "{path} is the workspace's token: isb delivers and rotates it (workspace_token_rotate)"
        ));
    }
    if write {
        if let Some(d) = MANAGED_DIRS.iter().find(|d| under(path, d)) {
            return Some(format!(
                "{d} is where isb delivers secrets and its own files; change them through secrets, apps or stacks"
            ));
        }
        if managed.iter().any(|m| m == path) {
            return Some(format!(
                "{path} is delivered by isb from an org secret; change the secret instead"
            ));
        }
    }
    None
}

/// An octal mode: `0644`, `"644"` or the number 420.
pub(super) fn parse_mode(v: &Value) -> Result<u32> {
    let bad = || Error::invalid("mode is an octal string like \"0644\"");
    let m = match v {
        Value::String(s) => {
            u32::from_str_radix(s.trim().trim_start_matches("0o"), 8).map_err(|_| bad())?
        }
        // JSON has no octal: 644 means 0644, as it does in YAML.
        Value::Number(n) => {
            let n = n.as_u64().ok_or_else(bad)?;
            u32::from_str_radix(&n.to_string(), 8).map_err(|_| bad())?
        }
        _ => return Err(bad()),
    };
    if m > 0o7777 {
        return Err(bad());
    }
    Ok(m)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileArgs {
    name: String,
    #[serde(default)]
    org: Option<String>,
    path: String,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    mode: Option<Value>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
    #[serde(default = "yes")]
    parents: bool,
}

fn yes() -> bool {
    true
}

/// The files isb delivers into `name`, when it is a stack replica.
fn managed_for(d: &Daemon, org: &OrgId, info: &SandboxInfo) -> Vec<String> {
    let labels = labels_of(info);
    match (labels.get("isb.stack"), labels.get("isb.service")) {
        (Some(stack), Some(svc)) => {
            let app = d.apps.get(org, svc).ok();
            managed_files(d, &crate::stack::qualified(org, stack), svc, app.as_ref())
        }
        _ => Vec::new(),
    }
}

fn instance_file_read(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: FileArgs = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let path = clean_path(&a.path)?;
    if let Some(why) = refuse_path(&path, false, &[]) {
        return Err(Error::Forbidden(why));
    }
    let want = a.encoding.as_deref().unwrap_or("auto");
    if !["auto", "utf8", "base64"].contains(&want) {
        return Err(Error::invalid("encoding is auto, utf8 or base64"));
    }
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    let body = oc
        .read_file(&a.name, &path)?
        .ok_or_else(|| Error::NotFound(format!("{path} in {}", a.name)))?;
    if body.len() > FILE_READ_CAP {
        return Err(Error::invalid(format!(
            "{path} is {} bytes; instance_file_read returns at most {} MiB (instance_exec with head, tail or split reads a part)",
            body.len(),
            FILE_READ_CAP / 1024 / 1024
        )));
    }
    let text = std::str::from_utf8(&body)
        .ok()
        .filter(|t| !t.contains('\0'));
    let (encoding, content) = match (want, text) {
        ("utf8", None) => {
            return Err(Error::invalid(format!(
                "{path} is not UTF-8 text; ask for encoding base64"
            )));
        }
        ("auto" | "utf8", Some(t)) => ("utf8", t.to_string()),
        _ => ("base64", crate::rpc::b64_encode(&body)),
    };
    let _ = info;
    Ok(
        json!({"instance": a.name, "path": path, "size": body.len(), "encoding": encoding, "content": content}),
    )
}

fn instance_file_write(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: FileArgs = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let path = clean_path(&a.path)?;
    let content = a.content.as_deref().ok_or_else(|| {
        Error::invalid("content is required (an empty string writes an empty file)")
    })?;
    let data = match a.encoding.as_deref().unwrap_or("utf8") {
        "utf8" => content.as_bytes().to_vec(),
        "base64" => crate::rpc::b64_decode(content)
            .map_err(|e| Error::invalid(format!("content is not base64: {e}")))?,
        _ => return Err(Error::invalid("encoding is utf8 or base64")),
    };
    if data.len() > FILE_WRITE_CAP {
        return Err(Error::invalid(format!(
            "{} bytes is over the {} MiB limit of instance_file_write",
            data.len(),
            FILE_WRITE_CAP / 1024 / 1024
        )));
    }
    if let Some(why) = refuse_path(&path, true, &managed_for(d, &org, &info)) {
        return Err(Error::Forbidden(why));
    }
    let mode = match &a.mode {
        Some(m) => parse_mode(m)?,
        None => 0o644,
    };
    let (uid, gid) = (a.uid.unwrap_or(0), a.gid.unwrap_or(0));
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    if a.parents {
        make_parents(&oc, &a.name, &path, uid, gid)?;
    }
    oc.push_file(&a.name, &path, &data, uid, gid, mode)?;
    Ok(
        json!({"instance": a.name, "path": path, "bytes": data.len(), "mode": format!("{mode:04o}"), "uid": uid, "gid": gid}),
    )
}

/// Create the directories above `path` that are missing (those that exist
/// keep their owner and mode).
fn make_parents(oc: &Client, instance: &str, path: &str, uid: u32, gid: u32) -> Result<()> {
    let mut dirs: Vec<String> = Vec::new();
    let mut cur = path
        .rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_default();
    while !cur.is_empty() {
        if oc.read_file(instance, &cur)?.is_some() {
            break;
        }
        dirs.push(cur.clone());
        cur = cur
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
    }
    for d in dirs.iter().rev() {
        oc.make_dir(instance, d, uid, gid, 0o755)?;
    }
    Ok(())
}

/// Register the file tools.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "instance_file_read",
        "Read a file in an instance",
        "Read one file of an instance, running or stopped (kubectl cp from it, for small files): at most 4 MiB. Text comes back as `utf8`, anything else as `base64` (or ask with `encoding`). It can hold secrets, so it is for members and up, and recorded in the audit log. Not the kernel's /proc, /sys, /dev, nor a workspace's token. For larger files run `head`, `tail` or `split` with instance_exec.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name."},
                "path": {"type": "string", "description": "Absolute path in the instance."},
                "encoding": {"type": "string", "enum": ["auto", "utf8", "base64"]}
            }),
            &["name", "path"]
        ),
        annotations("instance_file_read", ann),
        instance_file_read
    );
    tool!(
        r,
        d,
        "instance_file_write",
        "Write a file in an instance",
        "Write one file into an instance, running or stopped, replacing it (kubectl cp into it, for small files): at most 2 MiB, as utf8 text or base64, owned by uid/gid (default root) with `mode` (default 0644); missing parent directories are created unless parents=false. Refused: /run/isb, /run/secrets, /etc/isb, files isb delivers from org secrets (an app's `files`, a stack's secrets), the kernel's /proc, /sys, /dev. The audit log records the path and size, not the content. Members and up.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name."},
                "path": {"type": "string", "description": "Absolute path in the instance."},
                "content": {"type": "string"},
                "encoding": {"type": "string", "enum": ["utf8", "base64"]},
                "mode": {"type": "string", "description": "Octal, e.g. 0644."},
                "uid": {"type": "integer", "minimum": 0},
                "gid": {"type": "integer", "minimum": 0},
                "parents": {"type": "boolean", "description": "Create missing directories (default true)."}
            }),
            &["name", "path", "content"]
        ),
        annotations("instance_file_write", ann),
        instance_file_write
    );
    Ok(())
}
