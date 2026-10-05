//! Monitors isb makes by itself: which apps and compose stack services get
//! one of their own, what it is called, and how the org's list follows them.
//!
//! An app with a served domain gets `app-<name>`. A compose stack service
//! (`stack_deploy`) with a served domain gets `stack-<stack>-<service>`,
//! shortened with a hash when that is over 63 characters or already names
//! another stack service's monitor. Stacks that apps render (a project
//! environment's, a preview's) are left to the apps' monitors, and the
//! ingress tunnel's stack serves nothing of its own.
//!
//! A monitor is made once its target serves a domain, and kept while the
//! target exists and declares one, so a domain briefly in conflict or with
//! no replica neither loses the history nor makes a new monitor.

use super::{AUTO_PREFIX, Kind, MAX_PER_ORG, Monitor, STACK_PREFIX, Settings};
use crate::app::Apps;
use crate::org::OrgId;

/// Something that may get a monitor of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub target: Target,
    /// Its definition has domains: an existing monitor stays.
    pub declared: bool,
    /// The ingress serves one of them now: a missing monitor is made.
    pub served: bool,
}

/// An app, or a compose stack's service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    App(String),
    Service { stack: String, service: String },
}

impl Target {
    /// Is `m` this target's own monitor?
    fn owns(&self, m: &Monitor) -> bool {
        match self {
            Target::App(a) => m.kind == Kind::App && m.app.as_deref() == Some(a),
            Target::Service { stack, service } => {
                m.kind == Kind::Service
                    && m.stack.as_deref() == Some(stack)
                    && m.service.as_deref() == Some(service)
            }
        }
    }

    fn excluded(&self, s: &Settings) -> bool {
        match self {
            Target::App(a) => s.exclude_apps.contains(a),
            Target::Service { stack, service } => {
                s.exclude_services.contains(&exclusion(stack, service))
            }
        }
    }
}

/// How a stack service is named in the org's exclusions.
pub fn exclusion(stack: &str, service: &str) -> String {
    format!("{stack}/{service}")
}

/// The monitor a compose stack service gets: `stack-<stack>-<service>`
/// (the service as instance names spell it), or the hashed form when that
/// does not fit.
pub fn service_monitor_name(stack: &str, service: &str) -> String {
    let n = format!(
        "{STACK_PREFIX}{stack}-{}",
        crate::compose::sanitize_name(service)
    );
    if super::validate_name(&n).is_ok() {
        n
    } else {
        hashed_name(stack, service)
    }
}

/// `stack-<stack>-<service>` cut to fit, with a hash of the pair: unique
/// where the plain name is ambiguous (`a-b`/`c` and `a`/`b-c`).
pub fn hashed_name(stack: &str, service: &str) -> String {
    let h = super::service::fnv(&exclusion(stack, service)) & 0xff_ffff;
    let tail = format!("-{h:06x}");
    let base = format!(
        "{STACK_PREFIX}{stack}-{}",
        crate::compose::sanitize_name(service)
    );
    let head: String = base.chars().take(63 - tail.len()).collect();
    format!("{}{tail}", head.trim_end_matches('-'))
}

/// Does a served domain status count (the ingress routes it to replicas)?
fn serving(d: &crate::ingress::DomainStatus) -> bool {
    d.url.is_some() && matches!(d.state.as_str(), "serving" | "no-replicas")
}

/// The org's apps, as candidates.
pub fn apps(apps: &Apps, org: &OrgId) -> crate::error::Result<Vec<Candidate>> {
    let ctl = apps.controller();
    Ok(apps
        .list(org)?
        .into_iter()
        .map(|a| {
            let served = a.spec.stack().ok().is_some_and(|stack| {
                ctl.status(&crate::stack::qualified(org, &stack))
                    .ok()
                    .and_then(|s| s.services.into_iter().find(|x| x.service == a.spec.name))
                    .is_some_and(|s| s.domains.iter().any(|d| d.url.is_some()))
            });
            Candidate {
                declared: !a.spec.domains.is_empty(),
                served,
                target: Target::App(a.spec.name),
            }
        })
        .collect())
}

/// The org's compose stack services, as candidates: every service of a
/// stack no app renders, but the ingress tunnel's.
pub fn stack_services(apps: &Apps, org: &OrgId) -> Vec<Candidate> {
    let ctl = apps.controller();
    let mut out = Vec::new();
    for d in ctl.definitions().into_iter().filter(|d| d.org == *org) {
        if apps.managed_by(org, &d.name).is_some() || Apps::app_rendered(&d) {
            continue;
        }
        let status = ctl.status(&d.qualified()).ok();
        for (svc, spec) in &d.file.services {
            if spec.labels.contains_key(crate::app::LABEL_APP) {
                continue;
            }
            let served = status
                .as_ref()
                .and_then(|s| s.services.iter().find(|x| x.service == *svc))
                .is_some_and(|s| s.domains.iter().any(serving));
            out.push(Candidate {
                target: Target::Service {
                    stack: d.name.clone(),
                    service: svc.clone(),
                },
                declared: !spec.domains.is_empty(),
                served,
            });
        }
    }
    out
}

/// Bring the auto monitors in `all` in line with `cands`: drop those whose
/// target is gone, declares no domain or is excluded (all of them when the
/// org turned them off), make those missing for a served target. Returns
/// the names removed and the names made; `now` is unix seconds.
pub fn reconcile(
    all: &mut Vec<Monitor>,
    settings: &Settings,
    cands: &[Candidate],
    now: u64,
) -> (Vec<String>, Vec<String>) {
    let wanted =
        |c: &Candidate| settings.auto_monitors && c.declared && !c.target.excluded(settings);
    let mut gone = Vec::new();
    all.retain(|m| {
        let keep = !m.auto || cands.iter().any(|c| c.target.owns(m) && wanted(c));
        if !keep {
            gone.push(m.name.clone());
        }
        keep
    });
    let mut added = Vec::new();
    for c in cands.iter().filter(|c| wanted(c) && c.served) {
        if all.len() >= MAX_PER_ORG || all.iter().any(|m| m.auto && c.target.owns(m)) {
            continue;
        }
        let m = match &c.target {
            Target::App(a) => {
                let name = format!("{AUTO_PREFIX}{a}");
                // A monitor of that name the user made keeps it.
                if all.iter().any(|m| m.name == name) {
                    continue;
                }
                let mut m = Monitor::new(&name, Kind::App);
                m.app = Some(a.clone());
                m
            }
            Target::Service { stack, service } => {
                let taken = |n: &str| all.iter().any(|m| m.name == n);
                let plain = service_monitor_name(stack, service);
                let name = if !taken(&plain) {
                    plain
                } else {
                    let h = hashed_name(stack, service);
                    if taken(&h) {
                        continue;
                    }
                    h
                };
                let mut m = Monitor::new(&name, Kind::Service);
                (m.stack, m.service) = (Some(stack.clone()), Some(service.clone()));
                m
            }
        };
        let mut m = m;
        m.auto = true;
        (m.created_at, m.updated_at) = (now, now);
        added.push(m.name.clone());
        all.push(m);
    }
    (gone, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(stack: &str, service: &str, declared: bool, served: bool) -> Candidate {
        Candidate {
            target: Target::Service {
                stack: stack.into(),
                service: service.into(),
            },
            declared,
            served,
        }
    }

    fn app(name: &str, declared: bool, served: bool) -> Candidate {
        Candidate {
            target: Target::App(name.into()),
            declared,
            served,
        }
    }

    fn names(all: &[Monitor]) -> Vec<&str> {
        all.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn names_fit_and_never_look_like_an_apps() {
        assert_eq!(service_monitor_name("wiki", "web"), "stack-wiki-web");
        assert_eq!(service_monitor_name("wiki", "Web_UI"), "stack-wiki-web-ui");
        let long = service_monitor_name(
            "a-rather-long-stack-name-here",
            "an-even-longer-service-name-than-that",
        );
        assert_eq!(long.len(), 63, "{long}");
        assert!(long.starts_with("stack-a-rather-long-stack-name-here-an-even"));
        assert_eq!(
            long,
            service_monitor_name(
                "a-rather-long-stack-name-here",
                "an-even-longer-service-name-than-that"
            ),
            "deterministic"
        );
        super::super::validate_name(&long).unwrap();
        assert_ne!(hashed_name("a-b", "c"), hashed_name("a", "b-c"));
        for n in [hashed_name("a-b", "c"), service_monitor_name("x", "1")] {
            super::super::validate_name(&n).unwrap();
            assert!(n.starts_with(STACK_PREFIX) && !n.starts_with(AUTO_PREFIX));
        }
    }

    #[test]
    fn served_services_get_one_and_lose_it_with_their_domain() {
        let s = Settings::default();
        let mut all = Vec::new();
        let c = [
            svc("wiki", "web", true, true),
            svc("wiki", "redis", false, false),
            // Declared, not served yet (a conflict, refused, or starting).
            svc("blog", "ghost", true, false),
            app("shop", true, true),
        ];
        let (gone, added) = reconcile(&mut all, &s, &c, 7);
        assert!(gone.is_empty());
        assert_eq!(added, ["stack-wiki-web", "app-shop"]);
        let m = &all[0];
        assert_eq!(m.kind, Kind::Service);
        assert_eq!(
            (m.stack.as_deref(), m.service.as_deref()),
            (Some("wiki"), Some("web"))
        );
        assert!(m.auto && m.created_at == 7);
        m.validate().unwrap();
        // Again: nothing to do.
        assert_eq!(reconcile(&mut all, &s, &c, 8), (vec![], vec![]));
        // Not served for a while, still declared: kept.
        let c2 = [svc("wiki", "web", true, false), app("shop", true, true)];
        assert_eq!(reconcile(&mut all, &s, &c2, 9), (vec![], vec![]));
        // The domain removed: gone. The stack removed: gone too.
        let c3 = [svc("wiki", "web", false, false), app("shop", true, true)];
        let (gone, _) = reconcile(&mut all, &s, &c3, 10);
        assert_eq!(gone, ["stack-wiki-web"]);
        assert_eq!(names(&all), ["app-shop"]);
    }

    #[test]
    fn exclusions_and_the_org_switch() {
        let mut s = Settings::default();
        let c = [svc("wiki", "web", true, true), app("shop", true, true)];
        let mut all = Vec::new();
        reconcile(&mut all, &s, &c, 1);
        s.exclude_services = vec![exclusion("wiki", "web")];
        let (gone, added) = reconcile(&mut all, &s, &c, 2);
        assert_eq!((gone, added), (vec!["stack-wiki-web".to_string()], vec![]));
        s.exclude_services.clear();
        s.auto_monitors = false;
        let (gone, _) = reconcile(&mut all, &s, &c, 3);
        assert_eq!(gone, ["app-shop"]);
        assert!(all.is_empty());
    }

    #[test]
    fn names_taken_by_others_are_respected() {
        let s = Settings::default();
        // The user's own monitor of that name stays theirs.
        let mut mine = Monitor::new("stack-a-b-c", Kind::Tcp);
        (mine.host, mine.port) = (Some("x".into()), Some(1));
        let mut all = vec![mine];
        // `a-b`/`c` and `a`/`b-c` would both be stack-a-b-c.
        let c = [svc("a-b", "c", true, true), svc("a", "b-c", true, true)];
        let (_, added) = reconcile(&mut all, &s, &c, 1);
        assert_eq!(added, [hashed_name("a-b", "c"), hashed_name("a", "b-c")]);
        assert_eq!(all.len(), 3);
        // Kept by what they follow, not by name.
        assert_eq!(reconcile(&mut all, &s, &c, 2), (vec![], vec![]));
        // An app's own monitor and a stack service's never collide.
        let mut all = Vec::new();
        let c = [app("wiki-web", true, true), svc("wiki", "web", true, true)];
        let (_, added) = reconcile(&mut all, &s, &c, 1);
        assert_eq!(added, ["app-wiki-web", "stack-wiki-web"]);
    }
}
