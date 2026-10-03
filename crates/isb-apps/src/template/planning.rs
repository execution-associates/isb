//! The steps of [`plan`](super::plan): the values given, the apps' names,
//! each variable's value, and each app's spec.

use super::*;

/// What planning collects across variables and apps.
#[derive(Default)]
pub(super) struct Out {
    pub notes: Vec<String>,
    pub secrets: Vec<PlannedSecret>,
    /// `https://host/path` for each concrete domain.
    pub urls: Vec<String>,
}

/// Every value given must be a variable the template takes as input.
pub(super) fn check_values(t: &Template, p: &Params) -> Result<()> {
    for k in p.values.keys() {
        match t.var(k) {
            None => {
                return Err(Error::invalid(format!(
                    "{k}: template {} has no such variable",
                    t.id
                )));
            }
            Some(v) if !v.is_input() => {
                return Err(Error::invalid(format!("{k} is computed; it cannot be set")));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Each template app's name in the org; no two may be the same.
pub(super) fn app_names(t: &Template, p: &Params) -> Result<BTreeMap<String, String>> {
    let main = t.main_key();
    let names: BTreeMap<String, String> = t
        .apps
        .iter()
        .map(|a| {
            (
                a.name.clone(),
                app_name(&p.instance, &a.name, main == Some(a.name.as_str())),
            )
        })
        .collect();
    let uniq: BTreeSet<&String> = names.values().collect();
    if uniq.len() != names.len() {
        return Err(Error::invalid(format!(
            "template {}: two apps would both be named {}; pick another name",
            t.id, p.instance
        )));
    }
    Ok(names)
}

/// Resolve every variable, in dependency order, into `r.vars`.
pub(super) fn resolve_vars(
    t: &Template,
    r: &mut Renderer,
    ctx: &Context,
    uses: &BTreeMap<String, Vec<String>>,
    out: &mut Out,
) -> Result<Vec<PlannedVar>> {
    let p = r.p;
    let mut planned_vars = Vec::new();
    for v in t.var_order()? {
        let given = p.values.get(&v.name).filter(|s| !s.is_empty());
        if let Some(g) = given {
            validate_input(v, g).map_err(|e| Error::invalid(format!("{}: {e}", v.name)))?;
        }
        let x = var_value(t, v, given, r, ctx, uses, &mut out.notes)?;
        let (value, source, secret, auto) = (x.value, x.source, x.secret, x.auto);
        if v.default.is_some() && source == "default" {
            if let Some(val) = &value {
                validate_input(v, val)
                    .map_err(|e| Error::invalid(format!("{} default: {e}", v.name)))?;
            }
        }
        if secret {
            let name = var_secret(&p.instance, &v.name);
            crate::secrets::validate_name(&name)?;
            out.secrets.push(PlannedSecret {
                name,
                holds: format!("variable {}", v.name),
                value: value.clone().unwrap_or_default(),
            });
        }
        planned_vars.push(PlannedVar {
            name: v.name.clone(),
            value: if secret { None } else { value.clone() },
            secret,
            source: source.into(),
        });
        r.vars.insert(
            v.name.clone(),
            Resolved {
                value,
                secret,
                auto,
            },
        );
    }
    Ok(planned_vars)
}

/// A variable's value and where it came from.
struct VarValue {
    value: Option<String>,
    /// `given`, `generated`, `default` or `computed`.
    source: &'static str,
    secret: bool,
    /// A generated domain: `host: auto`.
    auto: bool,
}

/// Variable `v`'s value: computed, given, generated or its default.
fn var_value(
    t: &Template,
    v: &Variable,
    given: Option<&String>,
    r: &Renderer,
    ctx: &Context,
    uses: &BTreeMap<String, Vec<String>>,
    notes: &mut Vec<String>,
) -> Result<VarValue> {
    let at = format!("variable {}", v.name);
    let mut secret = v.declared_secret();
    let mut auto = false;
    let (value, source): (Option<String>, &'static str) = if let Some(expr) = &v.value {
        let segs = r.render(expr, &at)?;
        secret |= has_secret(&segs);
        (Some(concat(&segs)), "computed")
    } else if v.kind == VarKind::Domain {
        let (value, source, generated) = domain_value(t, v, given, r, ctx, uses, notes)?;
        auto = generated;
        (value, source)
    } else if let Some(g) = given {
        (Some(g.clone()), "given")
    } else if v.kind.generated() {
        (Some(generate_value(v, r, ctx, &at)?), "generated")
    } else if let Some(d) = &v.default {
        let segs = r.render(d, &at)?;
        secret |= has_secret(&segs);
        (Some(concat(&segs)), "default")
    } else if v.required() {
        return Err(Error::invalid(format!(
            "{} is required{}",
            v.name,
            if v.description.is_empty() {
                String::new()
            } else {
                format!(" ({})", v.description)
            }
        )));
    } else {
        (Some(String::new()), "default")
    };
    Ok(VarValue {
        value,
        source,
        secret,
        auto,
    })
}

/// A domain variable: given, its default, or generated (true) from the
/// public address and the app it names; `None` when there is no address.
fn domain_value(
    t: &Template,
    v: &Variable,
    given: Option<&String>,
    r: &Renderer,
    ctx: &Context,
    uses: &BTreeMap<String, Vec<String>>,
    notes: &mut Vec<String>,
) -> Result<(Option<String>, &'static str, bool)> {
    let at = format!("variable {}", v.name);
    if let Some(h) = given.map(String::as_str).filter(|h| *h != "auto") {
        return Ok((Some(h.to_string()), "given", false));
    }
    let d = match &v.default {
        Some(d) => r.plain(d, &at)?,
        None => String::new(),
    };
    if !d.is_empty() && d != "auto" {
        return Ok((Some(d), "default", false));
    }
    let anchor = uses
        .get(&v.name)
        .and_then(|u| u.first())
        .map(String::as_str)
        .or(t.main_key())
        .unwrap_or(t.apps[0].name.as_str());
    let svc = &r.names[anchor];
    let host = match ctx.public_ip {
        Some(ip) => Some(crate::ingress::domain::auto_host(
            &r.p.org, &r.stack, svc, ip,
        )?),
        None => None,
    };
    if host.is_none() {
        notes.push(format!(
            "{}: a generated hostname needs a public address (isb serve --ingress-public-ip); until then the domain is not served",
            v.name
        ));
    }
    Ok((host, "generated", true))
}

/// The reference to a template variable's secret.
fn secret_ref(p: &Params, var: &str) -> Value {
    json!({"secret": var_secret(&p.instance, var)})
}

/// Template app `a` as the app spec a deploy creates.
pub(super) fn plan_app(
    a: &AppTemplate,
    r: &Renderer,
    ctx: &Context,
    uses: &BTreeMap<String, Vec<String>>,
    out: &mut Out,
) -> Result<AppSpec> {
    let name = r.names[&a.name].clone();
    // Secrets a command or health check uses, passed as variables.
    let mut tpl_env: BTreeSet<String> = BTreeSet::new();
    let mut env = app_env(a, r, &name, &mut out.secrets)?;
    let command = app_command(a, r, ctx, &mut tpl_env)?;
    let healthcheck = match &a.healthcheck {
        None => None,
        Some(h) => Some(app_healthcheck(a, h, r, &mut tpl_env)?),
    };
    for var in &tpl_env {
        env.insert(secret_env_name(var), secret_ref(r.p, var));
    }
    if !tpl_env.is_empty() {
        out.notes.push(format!(
            "app {name}: a secret in its command or healthcheck reaches it as a variable through /bin/sh"
        ));
    }
    let domains = app_domains(a, r, uses, out)?;
    let files = app_files(a, r, &name, &mut out.secrets)?;
    let parts = AppParts {
        name,
        env,
        domains,
        files,
        command,
        healthcheck,
    };
    app_spec(a, r, parts, &mut out.notes)
}

/// An app's environment: plain values, references to variables' secrets,
/// and a secret of its own for a value built from one.
fn app_env(
    a: &AppTemplate,
    r: &Renderer,
    name: &str,
    secrets: &mut Vec<PlannedSecret>,
) -> Result<serde_json::Map<String, Value>> {
    let p = r.p;
    let mut env = serde_json::Map::new();
    for (k, s) in &a.env {
        let segs = r.render(s, &format!("app {} env {k}", a.name))?;
        let v = match segs.as_slice() {
            [Seg::Secret { var, .. }] => secret_ref(p, var),
            _ if has_secret(&segs) => {
                let sname = format!("tpl.{}.{}.env.{k}", p.instance, a.name);
                crate::secrets::validate_name(&sname).map_err(|_| {
                    Error::invalid(format!(
                        "{}: env {k}: the name cannot make a secret name",
                        a.name
                    ))
                })?;
                secrets.push(PlannedSecret {
                    name: sname.clone(),
                    holds: format!("app {name} env {k}"),
                    value: concat(&segs),
                });
                json!({"secret": sname})
            }
            _ => json!(concat(&segs)),
        };
        env.insert(k.clone(), v);
    }
    Ok(env)
}

/// The command line: `command`, or the image's entrypoint and `args`; a
/// secret in it runs it through /bin/sh with the secret as a variable.
fn app_command(
    a: &AppTemplate,
    r: &Renderer,
    ctx: &Context,
    tpl_env: &mut BTreeSet<String>,
) -> Result<Option<Vec<String>>> {
    let at = |w: &str| format!("app {} {w}", a.name);
    let c = match (&a.command, &a.args) {
        (Some(c), _) | (None, Some(c)) => c,
        (None, None) => return Ok(None),
    };
    let mut words = argv(c, &at("command"))?;
    if a.args.is_some() {
        let image = r.plain(&a.image, &at("image"))?;
        let ep = (ctx.entrypoint)(&image).map_err(|e| {
            Error::invalid(format!(
                "app {}: args need the image's entrypoint, and reading {image} failed: {e}",
                a.name
            ))
        })?;
        let mut full = ep.unwrap_or_default();
        full.extend(words);
        words = full;
    }
    let segs: Vec<Vec<Seg>> = words
        .iter()
        .map(|w| r.render(w, &at("command")))
        .collect::<Result<_>>()?;
    if segs.iter().any(|s| has_secret(s)) {
        Ok(Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            shell_line(segs, tpl_env),
        ]))
    } else {
        Ok(Some(segs.iter().map(|s| concat(s)).collect()))
    }
}

/// The health check with its test and other values rendered.
fn app_healthcheck(
    a: &AppTemplate,
    h: &Value,
    r: &Renderer,
    tpl_env: &mut BTreeSet<String>,
) -> Result<Value> {
    let at = |w: &str| format!("app {} {w}", a.name);
    let mut h = h.clone();
    if let Some(test) = h.get("test").cloned() {
        h["test"] = healthcheck_test(&test, r, &at("healthcheck"), tpl_env)?;
    }
    if let Some(o) = h.as_object_mut() {
        for (k, v) in o.iter_mut() {
            if k != "test" {
                if let Some(s) = v.as_str() {
                    *v = json!(r.plain(s, &at("healthcheck"))?);
                }
            }
        }
    }
    Ok(h)
}

/// A health check's `test`, rendered; with a secret in it, a CMD-SHELL line
/// that reads the secret from a variable.
fn healthcheck_test(
    test: &Value,
    r: &Renderer,
    at: &str,
    tpl_env: &mut BTreeSet<String>,
) -> Result<Value> {
    let (shell, words): (bool, Vec<String>) = match test {
        Value::String(s) => (true, vec![s.clone()]),
        Value::Array(a) => {
            let w = argv(test, "healthcheck test")?;
            match a.first().and_then(Value::as_str) {
                Some("CMD-SHELL") => (true, w[1..].to_vec()),
                Some("CMD") => (false, w[1..].to_vec()),
                _ => (false, w),
            }
        }
        _ => (false, vec![]),
    };
    let segs: Vec<Vec<Seg>> = words
        .iter()
        .map(|w| r.render(w, at))
        .collect::<Result<_>>()?;
    Ok(if !segs.iter().any(|s| has_secret(s)) {
        match test {
            Value::String(_) => json!(concat(&segs[0])),
            _ => {
                let mut v: Vec<String> = Vec::new();
                if let Some(first) = test
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .filter(|f| matches!(*f, "CMD" | "CMD-SHELL" | "NONE"))
                {
                    v.push(first.into());
                }
                v.extend(segs.iter().map(|s| concat(s)));
                json!(v)
            }
        }
    } else if shell {
        json!(["CMD-SHELL", shell_test_line(&segs, tpl_env)])
    } else {
        let l = shell_line(segs, tpl_env);
        json!(["CMD-SHELL", l])
    })
}

/// A CMD-SHELL test's words joined back into its line, each secret as
/// `${VAR}`.
fn shell_test_line(segs: &[Vec<Seg>], tpl_env: &mut BTreeSet<String>) -> String {
    // Inside a shell line: the secret becomes a variable.
    let mut line = String::new();
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        for seg in s {
            match seg {
                Seg::Lit(l) => line.push_str(l),
                Seg::Secret { var, .. } => {
                    line.push_str(&format!("${{{}}}", secret_env_name(var)));
                    tpl_env.insert(var.clone());
                }
            }
        }
    }
    line
}

/// The app's domains, and the URLs they serve (added to `out.urls`).
fn app_domains(
    a: &AppTemplate,
    r: &Renderer,
    uses: &BTreeMap<String, Vec<String>>,
    out: &mut Out,
) -> Result<Vec<Value>> {
    let at = |w: &str| format!("app {} {w}", a.name);
    let mut domains = Vec::new();
    for d in &a.domains {
        let mut o = serde_json::Map::new();
        for (k, v) in d {
            let nv = match (k.as_str(), v.as_str()) {
                ("host", Some(h)) => domain_host(h, r, uses, &at("domain host"), &mut out.notes)?,
                (_, Some(s)) => json!(r.plain(s, &at(&format!("domain {k}")))?),
                _ => v.clone(),
            };
            o.insert(k.clone(), nv);
        }
        if let Some(url) = domain_url(d, &o, r) {
            if !out.urls.contains(&url) {
                out.urls.push(url);
            }
        }
        domains.push(Value::Object(o));
    }
    Ok(domains)
}

/// A domain's host: `auto` for a generated name one app uses, the generated
/// name written out when several share it, else the rendered host.
fn domain_host(
    h: &str,
    r: &Renderer,
    uses: &BTreeMap<String, Vec<String>>,
    at: &str,
    notes: &mut Vec<String>,
) -> Result<Value> {
    let parts = parse_expr(h).unwrap_or_default();
    Ok(match parts.as_slice() {
        [Part::Var(var)]
            if r.vars.get(var).is_some_and(|x| x.auto)
                && uses.get(var).is_some_and(|u| u.len() == 1) =>
        {
            json!("auto")
        }
        [Part::Var(var)] if r.vars.get(var).is_some_and(|x| x.auto) => {
            // Shared by several apps: one generated name for all (the
            // first app's).
            match &r.vars[var].value {
                Some(v) => {
                    notes.push(format!(
                        "{var}: the generated name {v} is shared by several apps, so it is written out; an org with a domain allowlist needs it listed"
                    ));
                    json!(v)
                }
                None => json!("auto"),
            }
        }
        _ => json!(r.plain(h, at)?),
    })
}

/// The URL a rendered domain `o` (from template domain `d`) serves, when
/// its host is known.
fn domain_url(
    d: &serde_json::Map<String, Value>,
    o: &serde_json::Map<String, Value>,
    r: &Renderer,
) -> Option<String> {
    let host = o.get("host").and_then(Value::as_str).unwrap_or("");
    let shown = if host == "auto" {
        d.get("host")
            .and_then(Value::as_str)
            .and_then(|h| match parse_expr(h).ok()?.as_slice() {
                [Part::Var(v)] => r.vars.get(v)?.value.clone(),
                _ => None,
            })
    } else {
        Some(host.to_string())
    };
    let h = shown?;
    let https = o.get("https").and_then(Value::as_bool).unwrap_or(true);
    let path = o.get("path").and_then(Value::as_str).unwrap_or("/");
    Some(format!(
        "{}://{h}{path}",
        if https { "https" } else { "http" }
    ))
}

/// The app's files, each kept as a secret of its own.
fn app_files(
    a: &AppTemplate,
    r: &Renderer,
    name: &str,
    secrets: &mut Vec<PlannedSecret>,
) -> Result<Vec<Value>> {
    let p = r.p;
    let at = |w: &str| format!("app {} {w}", a.name);
    let mut files = Vec::new();
    for (i, f) in a.files.iter().enumerate() {
        let path = r.plain(&f.path, &at("file path"))?;
        let sname = format!("tpl.{}.{}.file{}", p.instance, a.name, i + 1);
        crate::secrets::validate_name(&sname)?;
        let content = concat(&r.render(&f.content, &at(&format!("file {path}")))?);
        secrets.push(PlannedSecret {
            name: sname.clone(),
            holds: format!("app {name} file {path}"),
            value: content,
        });
        files.push(json!({"path": path, "secret": sname, "mode": f.mode.clone().unwrap_or_else(|| "0444".into())}));
    }
    Ok(files)
}

/// What [`plan_app`] has rendered so far.
struct AppParts {
    name: String,
    env: serde_json::Map<String, Value>,
    domains: Vec<Value>,
    files: Vec<Value>,
    command: Option<Vec<String>>,
    healthcheck: Option<Value>,
}

/// The app spec, from the parts and the rest of the template app.
fn app_spec(
    a: &AppTemplate,
    r: &Renderer,
    x: AppParts,
    notes: &mut Vec<String>,
) -> Result<AppSpec> {
    let p = r.p;
    let at = |w: &str| format!("app {} {w}", a.name);
    let mut spec = json!({
        "name": x.name,
        "project": p.project,
        "environment": p.environment,
        "source": {"image": r.plain(&a.image, &at("image"))?},
    });
    if !x.env.is_empty() {
        spec["env"] = Value::Object(x.env);
    }
    if let Some(port) = a.port {
        spec["port"] = json!(port);
    }
    if !x.domains.is_empty() {
        spec["domains"] = json!(x.domains);
    }
    let vols: Vec<String> = a
        .volumes
        .iter()
        .map(|v| r.plain(v, &at("volumes")))
        .collect::<Result<_>>()?;
    if !vols.is_empty() {
        spec["volumes"] = json!(vols);
    }
    let ports: Vec<String> = a
        .ports
        .iter()
        .map(|v| r.plain(v, &at("ports")))
        .collect::<Result<_>>()?;
    if !ports.is_empty() {
        spec["ports"] = json!(ports);
    }
    if let Some(n) = a.replicas {
        spec["replicas"] = json!(n);
    }
    if let Some(c) = x.command {
        spec["command"] = json!(c);
    }
    if let Some(h) = x.healthcheck {
        spec["healthcheck"] = h;
    }
    if let Some(res) = &a.resources {
        spec["resources"] = app_resources(res, r, &x.name, &at("resources"), notes)?;
    }
    if !x.files.is_empty() {
        spec["files"] = json!(x.files);
    }
    if let Some(u) = &a.user {
        spec["user"] = json!(r.plain(u, &at("user"))?);
    }
    if let Some(w) = &a.working_dir {
        spec["working_dir"] = json!(r.plain(w, &at("working_dir"))?);
    }
    let spec: AppSpec =
        serde_json::from_value(spec).map_err(|e| Error::invalid(format!("app {}: {e}", a.name)))?;
    spec.validate()
        .map_err(|e| Error::invalid(format!("app {}: {e}", a.name)))?;
    Ok(spec)
}

/// Resource limits, rendered; isb pins whole CPUs, so a fraction is
/// rounded up.
fn app_resources(
    res: &Resources,
    r: &Renderer,
    name: &str,
    at: &str,
    notes: &mut Vec<String>,
) -> Result<Value> {
    let mut out = serde_json::Map::new();
    if let Some(c) = &res.cpus {
        let c = r.plain(c, at)?;
        // isb pins whole CPUs.
        let c = match c.parse::<f64>() {
            Ok(f) if f > 0.0 && f.fract() != 0.0 => {
                notes.push(format!(
                    "app {name}: cpus {c} is rounded up to {}",
                    f.ceil()
                ));
                (f.ceil() as u64).to_string()
            }
            _ => c,
        };
        out.insert("cpus".into(), json!(c));
    }
    if let Some(m) = &res.memory {
        out.insert("memory".into(), json!(r.plain(m, at)?));
    }
    Ok(Value::Object(out))
}
