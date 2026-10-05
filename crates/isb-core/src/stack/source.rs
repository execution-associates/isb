//! A compose stack's source and what the daemon adds to it at deploy: the
//! stack's environment (variables `${VAR}` resolves against, kept apart
//! from the file) and its managed domains (domain records kept apart from
//! the file, per service). The file is stored as written; these are merged
//! into the copy that runs.
//!
//! A variable whose value is a secret reaches a service only as the whole
//! value of one of its environment variables (`KEY: ${VAR}`), which is
//! delivered as a store secret (`KEY: {secret: ...}`), so the value never
//! enters the stored file. Used anywhere else it is refused.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml_ng::{Mapping, Value};

use crate::error::{Error, Result};
use crate::spec::{ComposeFile, DomainSpec};

/// The top-level secret key an environment variable's secret `name`
/// renders to.
pub fn env_secret_key(name: &str) -> String {
    format!("env.{name}")
}

/// The variable `s` is exactly a reference to (`${VAR}` or `$VAR`).
fn whole_ref(s: &str) -> Option<&str> {
    let name = s
        .strip_prefix("${")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| s.strip_prefix('$'))?;
    let ok = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    ok.then_some(name)
}

/// The services' environment-like lists (`environment`, `exec.env`).
fn env_blocks(v: &mut Value) -> Vec<&mut Value> {
    let mut out = Vec::new();
    let Some(Value::Mapping(services)) = v.get_mut("services") else {
        return out;
    };
    for (_, svc) in services.iter_mut() {
        let Value::Mapping(svc) = svc else { continue };
        for (k, e) in svc.iter_mut() {
            match (k.as_str(), e) {
                (Some("environment"), e) => out.push(e),
                (Some("exec"), Value::Mapping(exec)) => {
                    if let Some(e) = exec.get_mut("env") {
                        out.push(e);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn walk_strings(v: &Value, f: &mut dyn FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Sequence(seq) => seq.iter().for_each(|x| walk_strings(x, f)),
        Value::Mapping(m) => {
            for (k, x) in m {
                walk_strings(k, f);
                walk_strings(x, f);
            }
        }
        Value::Tagged(t) => walk_strings(&t.value, f),
        _ => {}
    }
}

/// Every variable a compose document refers to: `${VAR}` anywhere, and a
/// bare `VAR` in an environment list (which takes the variable's value).
pub fn referenced_vars(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_strings(v, &mut |s| out.extend(crate::interp::references(s)));
    let mut v = v.clone();
    for e in env_blocks(&mut v) {
        if let Value::Sequence(items) = e {
            out.extend(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.contains('='))
                    .map(String::from),
            );
        }
    }
    out
}

/// Deliver the variables in `secret_vars` (variable to store secret name)
/// as secrets: a service environment variable whose whole value is one of
/// them becomes `{secret: env.<name>}`, with that top-level secret
/// declared (external, from the store). `bare` gives a bare list entry's
/// value when its variable is no secret. A secret variable used anywhere
/// else is an error naming it. Returns whether `v` changed.
pub fn deliver_secret_vars(
    v: &mut Value,
    secret_vars: &BTreeMap<String, String>,
    bare: &dyn Fn(&str) -> Option<String>,
) -> Result<bool> {
    if secret_vars.is_empty() {
        return Ok(false);
    }
    let mut used: BTreeSet<String> = BTreeSet::new();
    for e in env_blocks(v) {
        // A list is made a map when it has a secret in it.
        if let Value::Sequence(items) = e {
            let hit = items.iter().filter_map(Value::as_str).any(|s| {
                let val = s.split_once('=').map_or(s, |(_, v)| v);
                let var = if s.contains('=') {
                    whole_ref(val)
                } else {
                    Some(s)
                };
                var.is_some_and(|x| secret_vars.contains_key(x))
            });
            if !hit {
                continue;
            }
            let mut m = Mapping::new();
            for s in items.iter().filter_map(Value::as_str) {
                match s.split_once('=') {
                    Some((k, val)) => {
                        m.insert(k.into(), val.into());
                    }
                    None if secret_vars.contains_key(s) => {
                        m.insert(s.into(), format!("${{{s}}}").into());
                    }
                    None => {
                        if let Some(val) = bare(s) {
                            m.insert(s.into(), val.into());
                        }
                    }
                }
            }
            *e = Value::Mapping(m);
        }
        let Value::Mapping(m) = e else { continue };
        for (_, val) in m.iter_mut() {
            let Some(var) = val.as_str().and_then(whole_ref) else {
                continue;
            };
            let Some(name) = secret_vars.get(var) else {
                continue;
            };
            let key = env_secret_key(name);
            let mut sec = Mapping::new();
            sec.insert("secret".into(), key.as_str().into());
            *val = Value::Mapping(sec);
            used.insert(name.clone());
        }
    }
    // Anything left would put the value into the file.
    let mut misused: Option<String> = None;
    walk_strings(v, &mut |s| {
        if misused.is_none() {
            misused = crate::interp::references(s)
                .into_iter()
                .find(|r| secret_vars.contains_key(r));
        }
    });
    if let Some(var) = misused {
        return Err(Error::invalid(format!(
            "variable {var} is the secret ${{{{secret.{}}}}}: a secret is only delivered as the whole value of a service's environment variable (KEY: ${{{var}}}), never written into the file",
            secret_vars[&var]
        )));
    }
    if used.is_empty() {
        return Ok(false);
    }
    let Value::Mapping(top) = v else {
        return Ok(true);
    };
    let secrets = top
        .entry("secrets".into())
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if secrets.is_null() {
        *secrets = Value::Mapping(Mapping::new());
    }
    let Value::Mapping(secrets) = secrets else {
        return Err(Error::invalid("top-level secrets: expected a mapping"));
    };
    for name in used {
        let key = env_secret_key(&name);
        if secrets.contains_key(key.as_str()) {
            return Err(Error::invalid(format!(
                "top-level secret {key:?} is the stack environment's; name yours otherwise"
            )));
        }
        let mut d = Mapping::new();
        d.insert("external".into(), true.into());
        d.insert("name".into(), name.as_str().into());
        secrets.insert(key.into(), Value::Mapping(d));
    }
    Ok(true)
}

/// A domain's identity for clashes: its host, lowercased.
fn host_of(d: &DomainSpec) -> String {
    d.host.trim().to_ascii_lowercase()
}

/// Add the stack's managed domains to the services they are for. A
/// hostname the file already gives any service, or that the managed
/// records claim twice (same host and path), is an error naming it.
/// Records for a service the file does not have are kept but unused.
pub fn merge_domains(
    file: &mut ComposeFile,
    managed: &BTreeMap<String, Vec<DomainSpec>>,
) -> Result<()> {
    let in_file: BTreeMap<String, String> = file
        .services
        .iter()
        .flat_map(|(svc, s)| s.domains.iter().map(move |d| (host_of(d), svc.clone())))
        .collect();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (svc, domains) in managed {
        let Some(spec) = file.services.get_mut(svc) else {
            continue;
        };
        for d in domains {
            let host = host_of(d);
            if host != "auto" {
                if let Some(owner) = in_file.get(&host) {
                    return Err(Error::invalid(format!(
                        "domain {host} is in the compose file (service {owner}) and in the stack's domains (service {svc}); keep one"
                    )));
                }
            }
            let path = d.path.clone().unwrap_or_else(|| "/".into());
            if !seen.insert((format!("{svc}\0{host}"), path.clone()))
                || (host != "auto" && !seen.insert((host.clone(), path.clone())))
            {
                return Err(Error::invalid(format!(
                    "domain {host}{} is given twice in the stack's domains",
                    if path == "/" { "" } else { path.as_str() }
                )));
            }
            spec.domains.push(d.clone());
        }
    }
    Ok(())
}

/// The domains the file itself gives each service: what is deployed less
/// what was merged in from the managed records.
pub fn file_domains(
    file: &ComposeFile,
    merged: &BTreeMap<String, Vec<DomainSpec>>,
) -> BTreeMap<String, Vec<DomainSpec>> {
    file.services
        .iter()
        .map(|(svc, s)| {
            let mut ds = s.domains.clone();
            for m in merged.get(svc).into_iter().flatten() {
                if let Some(i) = ds.iter().rposition(|d| d == m) {
                    ds.remove(i);
                }
            }
            (svc.clone(), ds)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    #[test]
    fn referenced_vars_cover_strings_keys_and_bare_entries() {
        let v = yaml(
            "services:\n  a:\n    image: ${IMG:-x}\n    environment: [PLAIN, 'K=${V}']\n    labels: {'${L}': '1'}\n",
        );
        let r: Vec<String> = referenced_vars(&v).into_iter().collect();
        assert_eq!(r, ["IMG", "L", "PLAIN", "V"]);
    }

    #[test]
    fn a_secret_variable_becomes_a_store_secret() {
        let mut v = yaml(
            "services:\n  a:\n    image: docker:nginx\n    environment:\n      DB: ${PW}\n      HOST: ${HOST}\n  b:\n    image: docker:nginx\n    environment: [PW, OTHER, K=v]\n",
        );
        let sv = BTreeMap::from([("PW".to_string(), "db.pw".to_string())]);
        let bare = |k: &str| (k == "OTHER").then(|| "o".to_string());
        assert!(deliver_secret_vars(&mut v, &sv, &bare).unwrap());
        assert_eq!(
            v["services"]["a"]["environment"]["DB"]["secret"],
            "env.db.pw"
        );
        // Plain references are left for interpolation.
        assert_eq!(v["services"]["a"]["environment"]["HOST"], "${HOST}");
        assert_eq!(
            v["services"]["b"]["environment"]["PW"]["secret"],
            "env.db.pw"
        );
        assert_eq!(v["services"]["b"]["environment"]["OTHER"], "o");
        assert_eq!(v["services"]["b"]["environment"]["K"], "v");
        assert_eq!(v["secrets"]["env.db.pw"]["name"], "db.pw");
        assert_eq!(v["secrets"]["env.db.pw"]["external"], true);
        // It loads, and the value is nowhere in the file.
        let p = crate::compose::load_docs(
            &[("c.yaml".into(), serde_yaml_ng::to_string(&v).unwrap())],
            std::path::Path::new("/srv"),
            Some("s"),
            &|k| (k == "HOST").then(|| "h".into()),
        )
        .unwrap();
        assert_eq!(p.file.services["a"].env.secrets["DB"], "env.db.pw");
        assert_eq!(p.file.services["a"].env["HOST"], "h");
    }

    #[test]
    fn a_secret_variable_inside_a_value_is_refused() {
        let sv = BTreeMap::from([("PW".to_string(), "pw".to_string())]);
        for doc in [
            "services:\n  a:\n    image: x\n    environment: {URL: 'pg://u:${PW}@db'}\n",
            "services:\n  a:\n    image: x\n    command: [run, '${PW}']\n",
        ] {
            let mut v = yaml(doc);
            let e = deliver_secret_vars(&mut v, &sv, &|_| None)
                .unwrap_err()
                .to_string();
            assert!(
                e.contains("variable PW is the secret ${{secret.pw}}"),
                "{e}"
            );
        }
        // No secret variables: nothing to do.
        let mut v = yaml("services: {a: {image: '${PW}'}}\n");
        assert!(!deliver_secret_vars(&mut v, &BTreeMap::new(), &|_| None).unwrap());
    }

    fn file(s: &str) -> ComposeFile {
        crate::compose::load_docs(
            &[("c.yaml".into(), s.to_string())],
            std::path::Path::new("/srv"),
            Some("s"),
            &|_| None,
        )
        .unwrap()
        .file
    }

    fn dom(host: &str) -> DomainSpec {
        DomainSpec {
            host: host.into(),
            port: Some(80),
            ..Default::default()
        }
    }

    #[test]
    fn managed_domains_merge_and_clashes_are_named() {
        let f = file(
            "services:\n  web:\n    image: x\n    domains: [{host: a.example.com, port: 80}]\n  api:\n    image: x\n",
        );
        let managed = BTreeMap::from([
            ("api".to_string(), vec![dom("b.example.com")]),
            ("gone".to_string(), vec![dom("c.example.com")]),
        ]);
        let mut g = f.clone();
        merge_domains(&mut g, &managed).unwrap();
        assert_eq!(g.services["api"].domains, [dom("b.example.com")]);
        assert_eq!(g.services["web"].domains.len(), 1);
        let fd = file_domains(&g, &managed);
        assert!(fd["api"].is_empty());
        assert_eq!(fd["web"][0].host, "a.example.com");
        // In both.
        let both = BTreeMap::from([("api".to_string(), vec![dom("A.example.com")])]);
        let e = merge_domains(&mut f.clone(), &both)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("domain a.example.com is in the compose file (service web)"),
            "{e}"
        );
        // Claimed twice.
        let twice = BTreeMap::from([
            ("api".to_string(), vec![dom("d.example.com")]),
            ("web".to_string(), vec![dom("d.example.com")]),
        ]);
        let e = merge_domains(&mut f.clone(), &twice)
            .unwrap_err()
            .to_string();
        assert!(e.contains("domain d.example.com is given twice"), "{e}");
    }
}
