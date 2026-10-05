//! One Coolify compose service as an app. The parts isb treats the same as a
//! Dokploy service (images, ports, commands, health checks, users,
//! resources) are Dokploy's; what differs is here: the environment (magic
//! variables), file contents in `volumes:`, and where domains come from.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use serde_yaml_ng::Value as Y;

use super::magic::{Cx, Mode, classify};
use crate::template::dokploy::service as shared_svc;
use crate::template::shared::{Tx, key_name, pairs, yget, yscalar};
use crate::template::{AppTemplate, FileTemplate};

/// What the services' translations add to as they go.
#[derive(Default)]
pub struct Acc {
    /// Volume to the services using it.
    pub vol_users: BTreeMap<String, BTreeSet<String>>,
}

/// The app for compose service `name` (app `key`), or `None` when it is not
/// deployed.
pub fn translate(
    cx: &Cx,
    acc: &mut Acc,
    name: &str,
    key: String,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> Option<AppTemplate> {
    let mut ip = |s: &str, tx: &mut Tx| cx.expr(s, Mode::Compose, tx);
    shared_svc::check_keys(name, m, tx);
    if yget(m, "labels").is_some() {
        tx.note(format!("{name}: its labels are not applied"));
    }
    if yget(m, "env_file").is_some() {
        tx.note(format!("{name}: env_file is ignored"));
    }
    let image = shared_svc::image(name, m, &mut ip, tx)?;
    if !shared_svc::keeps_running(name, m, false, tx) {
        return None;
    }
    let env = environment(cx, name, m, tx);
    let (volumes, files) = volumes(cx, &mut acc.vol_users, name, m, tx);
    let ports = shared_svc::ports(name, m, &mut ip, tx);
    let (command, args) = shared_svc::command_line(name, m, &mut ip, tx);
    let mut healthcheck = shared_svc::healthcheck(m, &mut ip, tx);
    // Docker runs a health check from the image's working directory; isb
    // runs it from /, where a relative path is not found and the app would
    // never be healthy.
    if let Some(c) = healthcheck.as_ref().and_then(relative_command) {
        tx.note(format!(
            "{name}: its health check ({c}) is dropped: it is a path relative to the image's working directory"
        ));
        healthcheck = None;
    }
    let depends_on = shared_svc::dependencies(cx.keys, name, &key, m, tx);
    let user = shared_svc::user(name, m, &mut ip, tx);
    let working_dir = yget(m, "working_dir").and_then(yscalar).map(|w| ip(&w, tx));
    let (replicas, res) = shared_svc::resources(name, m, &mut ip, tx);
    let (domains, port) = domains(cx, name, m, tx);
    let mut v = shared_svc::Values {
        env,
        command,
        args,
        healthcheck,
        files,
    };
    shared_svc::rewrite_service_names(cx.aliases, &key, &mut v);
    Some(AppTemplate {
        name: key,
        image,
        env: v.env,
        port,
        domains,
        volumes,
        ports,
        replicas,
        command: v.command,
        args: v.args,
        healthcheck: v.healthcheck,
        resources: (res.cpus.is_some() || res.memory.is_some()).then_some(res),
        files: v.files,
        user,
        working_dir,
        secret_on_change: None,
        depends_on,
    })
}

/// The service's `environment:`. A magic name with no value of its own
/// (`- SERVICE_URL_APP_3000`, `SERVICE_PASSWORD_DB:`) is set to its value; a
/// bare other name is the variable of that name.
fn environment(
    cx: &Cx,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let Some(e) = yget(m, "environment") else {
        return env;
    };
    for (k, v) in pairs(e) {
        if !k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            tx.refuse(format!(
                "{name}: environment variable {k:?} (isb's names are letters, digits and _)"
            ));
            continue;
        }
        let own = v.as_deref().filter(|v| !v.is_empty());
        let magic = classify(&k).is_some();
        // A magic URL or FQDN given a path is still the name's value.
        let path_only = magic && own.is_some_and(|p| p.starts_with('/'));
        let value = match own {
            Some(v) if !path_only => cx.expr(v, Mode::Compose, tx),
            Some(_) => cx.bare(&k, tx),
            None if magic || v.is_none() => cx.bare(&k, tx),
            None => String::new(),
        };
        env.insert(k, value);
    }
    env
}

/// `<mount>:ro` text for a native volume entry.
fn named(vn: &str, target: &str, ro: bool) -> String {
    format!("{vn}:{target}{}", if ro { ":ro" } else { "" })
}

/// Files with a `content:`, named volumes, and the directories Coolify
/// makes for relative binds.
fn volumes(
    cx: &Cx,
    vol_users: &mut BTreeMap<String, BTreeSet<String>>,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> (Vec<String>, Vec<FileTemplate>) {
    let mut volumes = Vec::new();
    let mut files = Vec::new();
    let mut anon = 0;
    let mut ip = |s: &str, tx: &mut Tx| cx.expr(s, Mode::Compose, tx);
    for v in yget(m, "volumes")
        .and_then(Y::as_sequence)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(f) = inline_file(cx, name, &v, tx) {
            files.extend(f);
            continue;
        }
        let Some((kind, source, target, ro)) = shared_svc::parse_volume(name, &v, &mut ip, tx)
        else {
            continue;
        };
        if target.contains('$') || !target.starts_with('/') {
            tx.refuse(format!(
                "{name}: mount target {target:?} is not an absolute path"
            ));
            continue;
        }
        let is_path =
            source.starts_with(['/', '.', '~']) || kind == "bind" || source.contains(['$', '/']);
        if kind == "anon" {
            anon += 1;
            let n = format!("anon-{anon}");
            tx.note(format!(
                "{name}: anonymous volume {target} becomes the named volume {n}"
            ));
            volumes.push(named(&n, &target, ro));
        } else if !is_path {
            vol_users
                .entry(source.clone())
                .or_default()
                .insert(name.to_string());
            volumes.push(named(&key_name(&source), &target, ro));
        } else if let Some(vn) = path_volume(name, &source, &target, tx) {
            vol_users
                .entry(format!("./{vn}"))
                .or_default()
                .insert(name.to_string());
            volumes.push(named(&vn, &target, ro));
        }
    }
    (volumes, files)
}

/// A bind mount's source that is a path: refused when it is the host's,
/// else a named volume for the directory Coolify makes.
fn path_volume(name: &str, source: &str, target: &str, tx: &mut Tx) -> Option<String> {
    let src = source.trim_end_matches('/');
    if src.contains("docker.sock") || src.contains("podman.sock") {
        tx.refuse(format!("{name}: a container runtime socket ({source})"));
    } else if matches!(
        src,
        "/etc/localtime" | "/etc/timezone" | "/usr/share/zoneinfo"
    ) {
        tx.note(format!(
            "{name}: the host's {source} is not mounted; the app keeps its image's time zone (set TZ)"
        ));
    } else if source.starts_with(['/', '~']) || source.contains('$') {
        tx.refuse(format!("{name}: host path {source}"));
    } else {
        let vn = key_name(source.trim_start_matches("./").trim_start_matches("../"));
        tx.note(format!(
            "{name}: bind {source} becomes the named volume {vn} (it starts as a copy of the image's {target})"
        ));
        return Some(vn);
    }
    None
}

/// A `volumes:` entry that carries its file's `content:`.
fn inline_file(cx: &Cx, name: &str, v: &Y, tx: &mut Tx) -> Option<Vec<FileTemplate>> {
    let vm = v.as_mapping()?;
    let content = yget(vm, "content")?;
    let text = match content {
        Y::String(s) => s.clone(),
        Y::Null => String::new(),
        other => yscalar(other).unwrap_or_default(),
    };
    let target = yget(vm, "target").and_then(yscalar).unwrap_or_default();
    let target = cx.expr(&target, Mode::Compose, tx);
    if target.contains('$') || !target.starts_with('/') {
        tx.refuse(format!(
            "{name}: file target {target:?} is not an absolute path"
        ));
        return Some(vec![]);
    }
    let text = cx.expr(&text, Mode::Content, tx);
    // A script is run, so it is executable.
    let mode = text.starts_with("#!").then(|| "0555".to_string());
    Some(vec![FileTemplate {
        path: target,
        content: text,
        mode,
    }])
}

/// The port a domain with none given goes to: what the service exposes,
/// else the template's `# port:`, else its first published container port.
fn default_port(m: &serde_yaml_ng::Mapping, hint: Option<u16>) -> Option<u16> {
    let first = |k: &str| -> Option<u16> {
        yget(m, k)?.as_sequence()?.iter().find_map(|p| {
            let s = yscalar(p)?;
            let t = s.split('/').next()?.rsplit(':').next()?.to_string();
            t.parse().ok()
        })
    };
    first("expose").or(hint).or_else(|| first("ports"))
}

/// The service's domains and the port the first one goes to.
fn domains(
    cx: &Cx,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> (Vec<serde_json::Map<String, Value>>, Option<u16>) {
    let hint = cx.port_hint;
    let mut out: Vec<serde_json::Map<String, Value>> = Vec::new();
    let mut seen = BTreeSet::new();
    for d in cx.decls.iter().filter(|d| d.service == name) {
        let Some(host) = cx.host_var(&d.name) else {
            continue;
        };
        // A name given with a port and without one is one domain.
        let sibling = cx
            .decls
            .iter()
            .find(|o| o.service == name && o.name == d.name && o.port.is_some())
            .and_then(|o| o.port);
        let port = d
            .port
            .or(sibling)
            .or_else(|| default_port(m, hint))
            .unwrap_or_else(|| {
                tx.note(format!(
                    "{name}: no port is given for its domain; 80 is assumed"
                ));
                80
            });
        let path = d.path.clone().filter(|p| p != "/");
        if !seen.insert((host.to_string(), port, path.clone())) {
            continue;
        }
        let mut e = serde_json::Map::new();
        e.insert("host".into(), json!(format!("${{{host}}}")));
        e.insert("port".into(), json!(port));
        if let Some(p) = path {
            tx.note(format!("{name}: the domain serves only the path {p}"));
            e.insert("path".into(), json!(p));
        }
        out.push(e);
    }
    let port = out
        .first()
        .and_then(|d| d["port"].as_u64())
        .map(|p| p as u16);
    (out, port)
}

/// The command a health check runs, when it is a relative path such as
/// `extra/healthcheck`.
fn relative_command(h: &Value) -> Option<String> {
    let test = h.get("test")?.as_array()?;
    let words: Vec<&str> = test.iter().filter_map(Value::as_str).collect();
    let first = match words.as_slice() {
        ["CMD-SHELL", line, ..] => line.split_whitespace().next()?,
        ["CMD", cmd, ..] => cmd,
        _ => return None,
    };
    let relative = first.contains('/') && !first.starts_with(['/', '$']);
    relative.then(|| first.to_string())
}
