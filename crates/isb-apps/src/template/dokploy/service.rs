//! One compose service's translation into an app: a function per compose
//! concern, in the order the notes and refusals are reported.

use super::*;

/// What every service's translation reads.
pub(super) struct Shared<'a> {
    /// The template's .env, as native expressions, and its order.
    pub env: &'a BTreeMap<String, String>,
    pub env_order: &'a [String],
    /// `[[config.mounts]]`: file path to content.
    pub mounts: &'a BTreeMap<String, String>,
    /// The compose file's top-level `volumes:`.
    pub declared_vols: &'a serde_yaml_ng::Mapping,
    /// Compose service name to app name, and every name a service answers to.
    pub keys: &'a BTreeMap<String, String>,
    pub aliases: &'a [(String, String)],
    /// `[[config.domains]]` and the Dokploy variables they may use.
    pub domains: &'a [(String, u16, String, String)],
    pub dvars: &'a BTreeMap<String, String>,
}

/// What the services' translations add to as they go.
pub(super) struct Acc {
    /// Variables used but not set.
    pub unset: BTreeSet<String>,
    /// Volume to the services using it.
    pub vol_users: BTreeMap<String, BTreeSet<String>>,
}

/// Interpolates a compose value against the template's .env.
type Ip<'a> = dyn FnMut(&str, &mut Tx) -> String + 'a;

/// The app for compose service `name` (app `key`), or None when it is
/// not deployed (no image, scale 0); refusals and notes go to `tx`.
pub(super) fn translate(
    sh: &Shared,
    acc: &mut Acc,
    name: &str,
    key: String,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> Option<AppTemplate> {
    let unset = &mut acc.unset;
    let mut ip = |s: &str, tx: &mut Tx| -> String {
        match interpolate(s, sh.env, unset) {
            Ok(v) => v,
            Err(e) => {
                tx.refuse(format!("{name}: {e}"));
                String::new()
            }
        }
    };
    check_keys(name, m, tx);
    let image = image(name, m, &mut ip, tx)?;
    if !keeps_running(name, m, tx) {
        return None;
    }
    let env = environment(sh, name, m, &mut ip, tx);
    let (volumes, files) = volumes(sh, &mut acc.vol_users, name, m, &mut ip, tx);
    let ports = ports(name, m, &mut ip, tx);
    let (command, args) = command_line(name, m, &mut ip, tx);
    let healthcheck = healthcheck(m, &mut ip, tx);
    let depends_on = dependencies(sh, name, &key, m, tx);
    let user = user(name, m, &mut ip, tx);
    let working_dir = yget(m, "working_dir").and_then(yscalar).map(|w| ip(&w, tx));
    let (replicas, res) = resources(name, m, &mut ip, tx);
    let (domains, port) = domains(sh, name, m, &mut ip, tx);
    let mut v = Values {
        env,
        command,
        args,
        healthcheck,
        files,
    };
    rewrite_service_names(sh, name, &key, &mut v, tx);
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
        depends_on,
    })
}

/// Refuse what isb cannot do and note what it ignores, key by key.
fn check_keys(name: &str, m: &serde_yaml_ng::Mapping, tx: &mut Tx) {
    for (k, v) in m {
        let k = yscalar(k).unwrap_or_default();
        if k.starts_with("x-") || IGNORED_KEYS.contains(&k.as_str()) {
            continue;
        }
        if let Some((_, why)) = REFUSED_KEYS.iter().find(|(r, _)| *r == k) {
            let harmless = match k.as_str() {
                "privileged" => v.as_bool() == Some(false),
                "pid" | "ipc" | "uts" | "userns_mode" | "cgroup" => {
                    yscalar(v).is_some_and(|s| s.is_empty() || s == "private" || s == "shareable")
                }
                "devices" | "cap_add" | "sysctls" | "extra_hosts" | "dns" | "dns_search" => {
                    matches!(v, Y::Sequence(s) if s.is_empty())
                        || matches!(v, Y::Mapping(m) if m.is_empty())
                        || v.is_null()
                }
                _ => false,
            };
            if !harmless {
                tx.refuse(format!("{name}: {why}"));
            }
        } else if let Some((_, why)) = NOTED_KEYS.iter().find(|(r, _)| *r == k) {
            tx.note(format!("{name}: {why}"));
        } else {
            tx.note(format!("{name}: compose key {k} is ignored"));
        }
    }
}

/// The service's image; None (refused unless it builds one) without one.
fn image(name: &str, m: &serde_yaml_ng::Mapping, ip: &mut Ip, tx: &mut Tx) -> Option<String> {
    match yget(m, "image").and_then(yscalar) {
        Some(i) => Some(image_ref(&ip(&i, tx))),
        None => {
            if yget(m, "build").is_none() {
                tx.refuse(format!("{name}: no image"));
            }
            None
        }
    }
}

/// Network mode and restart policy; false for `scale: 0` (not deployed).
fn keeps_running(name: &str, m: &serde_yaml_ng::Mapping, tx: &mut Tx) -> bool {
    // Network mode.
    if let Some(nm) = yget(m, "network_mode").and_then(yscalar) {
        if !matches!(nm.as_str(), "" | "bridge" | "default") {
            tx.refuse(format!("{name}: network_mode {nm}"));
        }
    }
    // Restart: a one-shot job would be restarted for ever.
    match yget(m, "restart").and_then(yscalar).as_deref() {
        Some("no") | Some("\"no\"") => {
            tx.refuse(format!(
                "{name}: restart \"no\" (a one-shot job; apps are kept running)"
            ));
        }
        Some("on-failure") => tx.note(format!(
            "{name}: restart on-failure becomes always (apps are kept running)"
        )),
        None => tx.note(format!("{name}: no restart policy; isb keeps it running")),
        _ => {}
    }
    if let Some(sc) = yget(m, "scale").and_then(yscalar) {
        if sc == "0" {
            tx.note(format!("{name}: scale 0; it is not deployed"));
            return false;
        }
    }
    true
}

/// env_file .env first, environment over it.
fn environment(
    sh: &Shared,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    ip: &mut Ip,
    tx: &mut Tx,
) -> BTreeMap<String, String> {
    let mut senv: BTreeMap<String, String> = BTreeMap::new();
    if let Some(ef) = yget(m, "env_file") {
        let files: Vec<String> = match ef {
            Y::String(s) => vec![s.clone()],
            Y::Sequence(s) => s
                .iter()
                .filter_map(|e| {
                    yscalar(e).or_else(|| ymap(e).and_then(|m| yget(m, "path")).and_then(yscalar))
                })
                .collect(),
            _ => vec![],
        };
        for f in files {
            if matches!(f.as_str(), ".env" | "./.env") {
                for k in sh.env_order {
                    senv.insert(k.clone(), sh.env[k].clone());
                }
            } else {
                tx.refuse(format!("{name}: env_file {f} (only Dokploy's .env exists)"));
            }
        }
    }
    if let Some(e) = yget(m, "environment") {
        for (k, v) in pairs(e) {
            match v {
                Some(v) => {
                    let x = ip(&v, tx);
                    senv.insert(k, x);
                }
                None => match sh.env.get(&k) {
                    Some(x) => {
                        senv.insert(k, x.clone());
                    }
                    None => tx.note(format!(
                        "{name}: environment {k} has no value (it is left out, as docker does)"
                    )),
                },
            }
        }
    }
    senv
}

/// A `volumes:` entry as (kind, source, target, read-only): kind is anon,
/// bind or auto (a name or a path, decided by the caller).
fn parse_volume(
    name: &str,
    v: &Y,
    ip: &mut Ip,
    tx: &mut Tx,
) -> Option<(&'static str, String, String, bool)> {
    let parsed = match v {
        Y::String(s) => {
            let s = ip(s, tx);
            let parts: Vec<&str> = s.split(':').collect();
            match parts.as_slice() {
                [t] => ("anon", String::new(), t.to_string(), false),
                [src, t] => ("auto", src.to_string(), t.to_string(), false),
                [src, t, o] => (
                    "auto",
                    src.to_string(),
                    t.to_string(),
                    o.split(',').any(|x| x == "ro"),
                ),
                _ => {
                    tx.refuse(format!("{name}: volume {s:?} does not parse"));
                    return None;
                }
            }
        }
        Y::Mapping(vm) => {
            let raw = |k: &str| yget(vm, k).and_then(yscalar);
            let t = raw("type").unwrap_or_else(|| "volume".into());
            let src = raw("source").map(|x| ip(&x, tx)).unwrap_or_default();
            let tgt = raw("target").map(|x| ip(&x, tx)).unwrap_or_default();
            let ro = yget(vm, "read_only").and_then(Y::as_bool).unwrap_or(false);
            match t.as_str() {
                "tmpfs" => {
                    tx.note(format!(
                        "{name}: tmpfs {tgt} is not made; it is on the root filesystem"
                    ));
                    return None;
                }
                "bind" | "volume" => {}
                other => {
                    tx.refuse(format!("{name}: a {other} mount"));
                    return None;
                }
            }
            let kind = if src.is_empty() {
                "anon"
            } else if t == "bind" {
                "bind"
            } else {
                "auto"
            };
            (kind, src, tgt, ro)
        }
        _ => {
            tx.refuse(format!("{name}: a volume entry does not parse"));
            return None;
        }
    };
    Some(parsed)
}

/// Named volumes, and the files `[[config.mounts]]` puts where the
/// service mounts them.
fn volumes(
    sh: &Shared,
    vol_users: &mut BTreeMap<String, BTreeSet<String>>,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    ip: &mut Ip,
    tx: &mut Tx,
) -> (Vec<String>, Vec<FileTemplate>) {
    let mut volumes = Vec::new();
    let mut files = Vec::new();
    let mut anon = 0;
    for v in yget(m, "volumes")
        .and_then(Y::as_sequence)
        .cloned()
        .unwrap_or_default()
    {
        let Some((kind, source, target, ro)) = parse_volume(name, &v, ip, tx) else {
            continue;
        };
        if target.contains('$') || !target.starts_with('/') {
            tx.refuse(format!(
                "{name}: mount target {target:?} is not an absolute path"
            ));
            continue;
        }
        let is_path = source.starts_with('/')
            || source.starts_with('.')
            || source.starts_with('~')
            || kind == "bind";
        if kind == "anon" {
            anon += 1;
            let n = format!("anon-{anon}");
            tx.note(format!(
                "{name}: anonymous volume {target} becomes the named volume {n}"
            ));
            volumes.push(format!("{n}:{target}{}", if ro { ":ro" } else { "" }));
        } else if !is_path {
            let vn = key_name(&source);
            vol_users
                .entry(source.clone())
                .or_default()
                .insert(name.to_string());
            if !sh.declared_vols.contains_key(Y::String(source.clone())) {
                tx.note(format!(
                    "{name}: volume {source} is not declared at the top level"
                ));
            }
            volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
        } else if let Some(rel) = source
            .strip_prefix("../files/")
            .or_else(|| source.strip_prefix("./files/"))
            .or_else(|| source.strip_prefix("files/"))
        {
            let rel = rel.trim_end_matches('/');
            let mut found = false;
            for (p, content) in sh.mounts {
                let tgt = if p == rel {
                    Some(target.clone())
                } else {
                    p.strip_prefix(&format!("{rel}/"))
                        .map(|sub| format!("{}/{sub}", target.trim_end_matches('/')))
                };
                if let Some(tgt) = tgt {
                    found = true;
                    files.push(FileTemplate {
                        path: tgt,
                        content: content.clone(),
                        mode: None,
                    });
                }
            }
            if !found {
                // A directory Dokploy makes in the deployment's files:
                // persistent storage, as a named volume.
                let vn = key_name(rel);
                vol_users
                    .entry(format!("files/{vn}"))
                    .or_default()
                    .insert(name.to_string());
                tx.note(format!(
                    "{name}: {source} (no content given) becomes the named volume {vn}"
                ));
                volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
            }
        } else if source.contains("docker.sock") {
            tx.refuse(format!("{name}: the docker socket ({source})"));
        } else if matches!(
            source.trim_end_matches('/'),
            "/etc/localtime" | "/etc/timezone" | "/usr/share/zoneinfo"
        ) {
            // The host's clock settings: not mounted (no host path is),
            // which only changes the default time zone.
            tx.note(format!(
                "{name}: the host's {source} is not mounted; the app keeps its image's time zone (set TZ)"
            ));
        } else if source.starts_with('/') || source.starts_with('~') {
            tx.refuse(format!("{name}: host path {source}"));
        } else {
            // A path in the compose project: per deployment, persistent.
            let vn = key_name(source.trim_start_matches("./").trim_start_matches("../"));
            vol_users
                .entry(format!("./{vn}"))
                .or_default()
                .insert(name.to_string());
            tx.note(format!(
                "{name}: bind {source} becomes the named volume {vn} (it starts as a copy of the image's {target})"
            ));
            volumes.push(format!("{vn}:{target}{}", if ro { ":ro" } else { "" }));
        }
    }
    (volumes, files)
}

/// Published ports, on 127.0.0.1 only.
fn ports(name: &str, m: &serde_yaml_ng::Mapping, ip: &mut Ip, tx: &mut Tx) -> Vec<String> {
    let mut ports = Vec::new();
    for p in yget(m, "ports")
        .and_then(Y::as_sequence)
        .cloned()
        .unwrap_or_default()
    {
        let (published, target, proto) = match &p {
            Y::Mapping(pm) => (
                yget(pm, "published").and_then(yscalar).map(|x| ip(&x, tx)),
                yget(pm, "target").and_then(yscalar).unwrap_or_default(),
                yget(pm, "protocol")
                    .and_then(yscalar)
                    .unwrap_or_else(|| "tcp".into()),
            ),
            other => {
                let s = ip(&yscalar(other).unwrap_or_default(), tx);
                let (s, proto) = match s.split_once('/') {
                    Some((a, b)) => (a.to_string(), b.to_string()),
                    None => (s, "tcp".into()),
                };
                let parts: Vec<&str> = s.rsplitn(2, ':').collect();
                match parts.as_slice() {
                    [t] => (None, t.to_string(), proto),
                    [t, rest] => (
                        Some(rest.rsplit(':').next().unwrap_or(rest).to_string()),
                        t.to_string(),
                        proto,
                    ),
                    _ => (None, String::new(), proto),
                }
            }
        };
        let ok_port = |s: &str| s.parse::<u16>().is_ok_and(|p| p > 0);
        match published.filter(|x| !x.is_empty()) {
            None => tx.note(format!(
                "{name}: port {target} without a host port is not published (docker would pick a random one)"
            )),
            Some(_) if proto != "tcp" => {
                tx.refuse(format!("{name}: a published {proto} port ({target}/{proto})"));
            }
            Some(h) if ok_port(&h) && ok_port(&target) => {
                tx.note(format!(
                    "{name}: port {h}:{target} is published on 127.0.0.1 only (isb's default)"
                ));
                ports.push(format!("127.0.0.1:{h}:{target}"));
            }
            Some(h) => tx.refuse(format!("{name}: port {h}:{target} (ranges are not supported)")),
        }
    }
    ports
}

/// `command`, or `entrypoint` and `command` as one command line.
fn command_line(
    name: &str,
    m: &serde_yaml_ng::Mapping,
    ip: &mut Ip,
    tx: &mut Tx,
) -> (Option<Value>, Option<Value>) {
    let entrypoint = yget(m, "entrypoint").map(words);
    let cmd = yget(m, "command").map(words);
    let mut command = None;
    let mut args = None;
    match (entrypoint, cmd) {
        (Some(None), _) | (_, Some(None)) => {
            tx.refuse(format!("{name}: command/entrypoint does not parse"));
        }
        (Some(Some(e)), c) => {
            let mut line: Vec<String> = e.iter().map(|w| ip(w, tx)).collect();
            if let Some(Some(c)) = c {
                line.extend(c.iter().map(|w| ip(w, tx)));
            }
            if !line.is_empty() {
                command = Some(json!(line));
            }
        }
        (None, Some(Some(c))) => {
            args = Some(json!(c.iter().map(|w| ip(w, tx)).collect::<Vec<_>>()));
        }
        (None, None) => {}
    }
    (command, args)
}

/// The health check, unless disabled.
fn healthcheck(m: &serde_yaml_ng::Mapping, ip: &mut Ip, tx: &mut Tx) -> Option<Value> {
    let mut healthcheck = None;
    if let Some(Y::Mapping(h)) = yget(m, "healthcheck") {
        let disabled = yget(h, "disable").and_then(Y::as_bool) == Some(true);
        let test = yget(h, "test");
        let none = matches!(test, Some(Y::Sequence(s)) if s.first().and_then(yscalar).as_deref() == Some("NONE"));
        if !disabled && !none {
            let mut out = serde_json::Map::new();
            match test {
                Some(Y::String(s)) => {
                    out.insert("test".into(), json!(ip(s, tx)));
                }
                Some(Y::Sequence(s)) => {
                    let w: Vec<String> = s.iter().filter_map(yscalar).map(|x| ip(&x, tx)).collect();
                    out.insert("test".into(), json!(w));
                }
                _ => {}
            }
            for f in ["interval", "timeout", "start_period", "start_interval"] {
                if let Some(v) = yget(h, f).and_then(yscalar) {
                    out.insert(f.into(), json!(ip(&v, tx)));
                }
            }
            if let Some(r) = yget(h, "retries").and_then(Y::as_u64) {
                out.insert("retries".into(), json!(r));
            }
            if out.contains_key("test") {
                healthcheck = Some(Value::Object(out));
            }
        }
    }
    healthcheck
}

/// depends_on and links, as the apps they name.
fn dependencies(
    sh: &Shared,
    name: &str,
    key: &str,
    m: &serde_yaml_ng::Mapping,
    tx: &mut Tx,
) -> Vec<String> {
    let mut depends = Vec::new();
    match yget(m, "depends_on") {
        Some(Y::Sequence(s)) => depends.extend(s.iter().filter_map(yscalar)),
        Some(Y::Mapping(dm)) => {
            for (d, c) in dm {
                let d = yscalar(d).unwrap_or_default();
                let cond = ymap(c).and_then(|c| yget(c, "condition")).and_then(yscalar);
                if cond.as_deref() == Some("service_completed_successfully") {
                    tx.refuse(format!(
                        "{name}: waits for {d} to complete (a one-shot job)"
                    ));
                }
                depends.push(d);
            }
        }
        _ => {}
    }
    if let Some(Y::Sequence(l)) = yget(m, "links") {
        tx.note(format!(
            "{name}: links become dependencies; names resolve as <app>.<stack>"
        ));
        for x in l.iter().filter_map(yscalar) {
            depends.push(x.split(':').next().unwrap_or(&x).to_string());
        }
    }
    let mut dep_keys = Vec::new();
    for d in depends {
        match sh.keys.get(&d) {
            Some(k) if !dep_keys.contains(k) && k != key => dep_keys.push(k.clone()),
            Some(_) => {}
            None => tx.note(format!("{name}: depends on {d}, which is not deployed")),
        }
    }
    dep_keys
}

/// The user: numeric, as an OCI image's must be.
fn user(name: &str, m: &serde_yaml_ng::Mapping, ip: &mut Ip, tx: &mut Tx) -> Option<String> {
    yget(m, "user")
        .and_then(yscalar)
        .map(|u| ip(&u, tx))
        .and_then(|u| {
            let (a, b) = u.split_once(':').unwrap_or((&u, ""));
            let num = |s: &str| s.is_empty() || s.parse::<u32>().is_ok();
            if a == "root" && (b.is_empty() || b == "root") {
                Some("0".to_string())
            } else if num(a) && num(b) && !a.is_empty() {
                Some(u.clone())
            } else {
                tx.refuse(format!(
                    "{name}: user {u:?} is a name; an OCI image's user must be numeric here"
                ));
                None
            }
        })
}

/// Replicas and resource limits (whole CPUs, isb's memory units).
fn resources(
    name: &str,
    m: &serde_yaml_ng::Mapping,
    ip: &mut Ip,
    tx: &mut Tx,
) -> (Option<u32>, Resources) {
    let mut replicas = None;
    let mut res = Resources::default();
    if let Some(Y::Mapping(d)) = yget(m, "deploy") {
        if let Some(r) = yget(d, "replicas").and_then(Y::as_u64) {
            replicas = Some(r as u32);
        }
        if yget(d, "mode").and_then(yscalar).as_deref() == Some("global") {
            tx.note(format!("{name}: deploy mode global runs one replica"));
        }
        if let Some(lim) = yget(d, "resources")
            .and_then(ymap)
            .and_then(|r| yget(r, "limits"))
            .and_then(ymap)
        {
            if let Some(c) = yget(lim, "cpus").and_then(yscalar) {
                res.cpus = Some(ip(&c, tx));
            }
            if let Some(mm) = yget(lim, "memory").and_then(yscalar) {
                res.memory = Some(ip(&mm, tx));
            }
        }
        for k in d.keys().filter_map(yscalar) {
            if !matches!(k.as_str(), "replicas" | "resources" | "mode") {
                tx.note(format!("{name}: deploy.{k} is ignored"));
            }
        }
    }
    if let Some(c) = yget(m, "cpus").and_then(yscalar) {
        res.cpus = Some(ip(&c, tx));
    }
    if let Some(mm) = yget(m, "mem_limit").and_then(yscalar) {
        res.memory = Some(ip(&mm, tx));
    }
    if let Some(c) = res.cpus.clone().filter(|c| !c.contains("${")) {
        match c.parse::<f64>() {
            Ok(f) if f > 0.0 => {
                let whole = f.ceil() as u64;
                if (whole as f64 - f).abs() > f64::EPSILON {
                    tx.note(format!(
                        "{name}: cpus {c} is rounded up to {whole} (isb pins whole CPUs)"
                    ));
                }
                res.cpus = Some(whole.to_string());
            }
            _ => {
                tx.refuse(format!("{name}: cpus {c:?}"));
                res.cpus = None;
            }
        }
    }
    if let Some(mm) = &res.memory {
        // docker's lower-case units; isb takes 512m, 2g, 1GiB.
        let t = mm.trim().to_ascii_lowercase();
        let t = t.trim_end_matches('b').to_string();
        res.memory = Some(t);
    }
    (replicas, res)
}

/// Domains from Traefik labels and `[[config.domains]]`, and the port
/// the first one routes to.
fn domains(
    sh: &Shared,
    name: &str,
    m: &serde_yaml_ng::Mapping,
    ip: &mut Ip,
    tx: &mut Tx,
) -> (Vec<serde_json::Map<String, Value>>, Option<u16>) {
    // Traefik labels.
    let labels: Vec<(String, String)> = yget(m, "labels")
        .map(pairs)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| {
            let v = v.unwrap_or_default();
            (k, ip(&v, tx))
        })
        .collect();
    let mut domains = traefik_domains(name, &labels, tx);
    if labels.iter().any(|(k, _)| !k.starts_with("traefik.")) {
        tx.note(format!("{name}: its labels are not applied"));
    }
    for (svc, port, host, path) in sh.domains {
        if svc == name {
            let mut d = serde_json::Map::new();
            d.insert("host".into(), json!(tx.expr(host, "domain", sh.dvars)));
            d.insert("port".into(), json!(port));
            if path != "/" {
                d.insert("path".into(), json!(path));
            }
            domains.push(d);
        }
    }
    // Domains on the same host and path once.
    let mut seen = BTreeSet::new();
    domains.retain(|d| {
        seen.insert((
            d.get("host").cloned().unwrap_or_default().to_string(),
            d.get("path").cloned().unwrap_or_default().to_string(),
        ))
    });
    let port = domains
        .first()
        .and_then(|d| d.get("port"))
        .and_then(Value::as_u64)
        .map(|p| p as u16);
    (domains, port)
}

/// A service's values that may name other services.
struct Values {
    env: BTreeMap<String, String>,
    command: Option<Value>,
    args: Option<Value>,
    healthcheck: Option<Value>,
    files: Vec<FileTemplate>,
}

/// Other services named in values: rewrite them to isb's service names.
fn rewrite_service_names(sh: &Shared, name: &str, key: &str, v: &mut Values, tx: &mut Tx) {
    let others: Vec<(String, String)> = sh
        .aliases
        .iter()
        .filter(|(_, k)| k != key)
        .cloned()
        .collect();
    let mut rewritten = BTreeSet::new();
    let mut fix = |e: &str, whole: bool| -> String {
        let (out, hits) = rewrite_hosts(e, &others, whole);
        rewritten.extend(hits);
        out
    };
    v.env = std::mem::take(&mut v.env)
        .into_iter()
        .map(|(k, v)| {
            let w = hostish_key(&k);
            (k, fix(&v, w))
        })
        .collect();
    let fix_args = |v: &Option<Value>, fix: &mut dyn FnMut(&str, bool) -> String| {
        v.as_ref().map(|v| match v {
            Value::Array(a) => json!(
                a.iter()
                    .map(|x| fix(x.as_str().unwrap_or(""), false))
                    .collect::<Vec<_>>()
            ),
            other => other.clone(),
        })
    };
    v.command = fix_args(&v.command, &mut fix);
    v.args = fix_args(&v.args, &mut fix);
    v.healthcheck = v.healthcheck.take().map(|mut h| {
        if let Some(t) = h.get("test").cloned() {
            h["test"] = match t {
                Value::String(s) => json!(fix(&s, false)),
                Value::Array(a) => json!(
                    a.iter()
                        .map(|x| fix(x.as_str().unwrap_or(""), false))
                        .collect::<Vec<_>>()
                ),
                o => o,
            };
        }
        h
    });
    for f in v.files.iter_mut() {
        f.content = fix(&f.content, false);
    }
    if !rewritten.is_empty() {
        tx.note(format!(
            "{name}: references to {} are rewritten to isb's service names (<app>.<stack>); names an image uses by default are not",
            rewritten.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}
