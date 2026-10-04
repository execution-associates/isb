//! Coolify's template format, translated into isb's.
//!
//! A Coolify template is one Docker Compose file, `templates/compose/<id>.yaml`
//! in `coollabsio/coolify`, with its metadata in comments at the top
//! (`# documentation:`, `# slogan:`, `# category:`, `# tags:`, `# logo:`,
//! `# port:`) and Coolify's "magic" environment variables in the compose
//! (see `magic`): `SERVICE_URL_*` / `SERVICE_FQDN_*` give a service a
//! domain, `SERVICE_PASSWORD_*` and friends are generated once, and
//! `${VAR:-default}` is an input with a default. A `volumes:` entry can carry
//! its file's `content:`.
//!
//! The translation is as strict as Dokploy's (and shares its compose rules,
//! [`super::dokploy`]): each compose service becomes one app, and anything
//! isb's model cannot express or that would weaken isolation (privileged,
//! capabilities, devices, host namespaces, host paths, the container
//! runtime's socket, one volume shared by several services, one-shot jobs) is
//! refused with a reason. What is mapped with a change of meaning is listed
//! in the report's notes; nothing is dropped silently.

mod magic;
mod service;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_more;

use std::collections::BTreeMap;

use serde::Serialize;

use self::magic::Cx;
use super::Template;
use super::shared::{
    Report, Tx, check_top_level, declared_volumes, finish, key_name, parse_compose, service_names,
    yget, ymap, yscalar,
};

pub use self::magic::{Gen, Magic, classify};

/// A template's metadata, from the comments at the top of its file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Meta {
    /// The file's name without `.yaml`.
    pub id: String,
    /// The id, titled (`uptime-kuma` is `Uptime Kuma`).
    pub name: String,
    /// The slogan.
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    /// The logo's https URL, when the catalog has a base to resolve it
    /// against.
    pub logo: Option<String>,
    /// The documentation link.
    pub docs: Option<String>,
    /// The port the template's main service listens on.
    pub port: Option<u16>,
    /// `# ignore: true`: Coolify does not offer it.
    pub ignore: bool,
}

/// The `# key: value` comments at the top of a template, in order.
pub fn header(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let Some(c) = l.strip_prefix('#') else { break };
        if let Some((k, v)) = c.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
                out.push((k, v.trim().to_string()));
            }
        }
    }
    out
}

/// `uptime-kuma-with-mysql` as `Uptime Kuma With Mysql`.
pub fn titled(id: &str) -> String {
    id.split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A logo path from the header, safe to put in a URL.
fn logo_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with(['/', '.'])
        && !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// A template's metadata. `logo_base` is where the repository's `public/`
/// directory is served from (a catalog URL), if anywhere.
pub fn meta(id: &str, text: &str, logo_base: Option<&str>) -> Meta {
    let h = header(text);
    let get = |k: &str| h.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    let mut tags: Vec<String> = vec![];
    for t in get("category")
        .into_iter()
        .chain(get("tags").unwrap_or("").split(','))
    {
        let t = t.trim().to_ascii_lowercase();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    Meta {
        id: id.to_string(),
        name: titled(id),
        description: get("slogan")
            .unwrap_or("")
            .trim_matches(['"', '\''])
            .trim()
            .to_string(),
        category: get("category").unwrap_or("").to_string(),
        tags,
        logo: get("logo")
            .filter(|l| logo_path_ok(l))
            .zip(logo_base)
            .map(|(l, b)| format!("{}/public/{l}", b.trim_end_matches('/'))),
        docs: get("documentation")
            .filter(|d| d.starts_with("https://") || d.starts_with("http://"))
            .map(String::from),
        port: get("port").and_then(|p| p.parse().ok()),
        ignore: get("ignore").is_some_and(|v| v.eq_ignore_ascii_case("true")),
    }
}

/// Translate one Coolify template. `None` with refusals when it cannot run
/// on isb.
pub fn translate(meta: &Meta, compose: &str) -> (Option<Template>, Report) {
    let mut tx = Tx {
        notes: vec![],
        refusals: vec![],
        vars: vec![],
        names: BTreeMap::new(),
    };
    let t = translate_inner(meta, compose, &mut tx);
    finish(t, tx)
}

fn translate_inner(meta: &Meta, compose: &str, tx: &mut Tx) -> Option<Template> {
    let doc = parse_compose(compose, tx)?;
    let Some(top) = ymap(&doc) else {
        tx.refuse("the compose file is not a mapping");
        return None;
    };
    check_top_level(top, tx);
    declared_volumes(top, tx);
    let Some(services) = yget(top, "services").and_then(ymap) else {
        tx.refuse("the compose file has no services");
        return None;
    };
    let (keys, aliases) = service_names(services, tx);
    let cx = Cx::new(&doc, services, (&keys, &aliases), meta.port, tx);
    let mut acc = service::Acc::default();
    let mut apps = Vec::new();
    for (name, s) in services {
        let name = yscalar(name).unwrap_or_default();
        let Some(key) = keys.get(&name).cloned() else {
            continue;
        };
        let Some(m) = ymap(s) else {
            tx.refuse(format!("service {name} is not a mapping"));
            continue;
        };
        if let Some(app) = service::translate(&cx, &mut acc, &name, key, m, tx) {
            apps.push(app);
        }
    }
    for (v, users) in &acc.vol_users {
        if users.len() > 1 {
            tx.refuse(format!(
                "volume {v} is shared by {} (an app's volumes are its own)",
                users.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    if apps.is_empty() {
        tx.refuse("no service to deploy");
        return None;
    }
    cx.finish(tx);
    let main = cx
        .decls
        .iter()
        .find_map(|d| keys.get(&d.service))
        .cloned()
        .unwrap_or_else(|| apps[0].name.clone());
    Some(Template {
        id: key_name(&meta.id),
        name: meta.name.clone(),
        description: meta.description.clone(),
        version: String::new(),
        logo: meta.logo.clone(),
        tags: meta.tags.clone(),
        links: meta
            .docs
            .iter()
            .map(|d| ("docs".to_string(), d.clone()))
            .collect(),
        variables: tx.vars.clone(),
        apps,
        main: Some(main),
        notes: vec![],
    })
}
