//! An org's quotas: what is allocated against them, and incus' refusals
//! said in isb's terms.
//!
//! An org's `limits.cpu`, `limits.memory` and `limits.disk` are incus
//! project limits, and incus enforces them as budgets: the sum of what every
//! instance in the project is configured with (its `limits.cpu`, its
//! `limits.memory`, its root disk's `size`, and the volumes' sizes), stopped
//! instances included, against the limit. Nothing measures actual use. A
//! shared ceiling on what the org's instances use together would need a
//! parent cgroup per project, which incus does not offer.
//!
//! incus checks the budgets when an instance or a volume is created or
//! resized, and refuses with text such as `Reached maximum aggregate value
//! "2" for "limits.cpu" in project "isb-lab"`. The client turns any such
//! answer into [`Error::Invalid`] with [`explain`]'s message, so every path
//! that creates instances in an org (sandboxes, workspaces, apps, databases,
//! builds, stacks) says the same thing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::OrgId;
use crate::client::Client;
use crate::error::Error;

/// The root disk size isb gives an instance it creates in an org with
/// `limits.disk` when its spec sets none (incus refuses an instance without
/// one there); see [`super::disk`].
pub const DEFAULT_ROOT_SIZE: &str = "10GiB";

/// One of an org's limits, which `isb org update` can lift again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Limit {
    Cpus,
    Memory,
    Disk,
    Instances,
}

impl Limit {
    /// The incus project key.
    pub fn key(self) -> &'static str {
        match self {
            Limit::Cpus => "limits.cpu",
            Limit::Memory => "limits.memory",
            Limit::Disk => "limits.disk",
            Limit::Instances => "limits.instances",
        }
    }
}

/// One limit of an org: the limit, what its instances are allocated against
/// it (summed over every instance, stopped ones included), and what is left.
/// Bytes for memory and disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Budget {
    pub limit: i64,
    pub allocated: i64,
    pub free: i64,
}

impl Budget {
    /// `3 of 4 allocated, 1 free`, sizes as incus writes them.
    pub fn text(&self, in_bytes: bool) -> String {
        let f = |n: i64| if in_bytes { bytes(n) } else { n.to_string() };
        format!(
            "{} of {} allocated, {} free",
            f(self.allocated),
            f(self.limit),
            f(self.free)
        )
    }
}

/// The org's limited resources (`cpu`, `memory`, `disk`, `instances`) as
/// budgets, from `/1.0/projects/<p>/state`. A resource without a limit is
/// left out: incus does not total what nothing limits.
pub(crate) fn budgets(state: &Value) -> BTreeMap<String, Budget> {
    ["cpu", "memory", "disk", "instances"]
        .into_iter()
        .filter_map(|name| {
            let (limit, allocated) = usage(state, &format!("limits.{name}"))?;
            (limit >= 0).then(|| {
                let b = Budget {
                    limit,
                    allocated,
                    free: (limit - allocated).max(0),
                };
                (name.to_string(), b)
            })
        })
        .collect()
}

/// A project's budgets, read from incus; empty when its state can't be read.
pub(crate) fn read_budgets(c: &Client, project: &str) -> BTreeMap<String, Budget> {
    c.clone()
        .project("")
        .get(&format!(
            "/1.0/projects/{}/state",
            crate::client::encode_segment(project)
        ))
        .map(|s| budgets(&s))
        .unwrap_or_default()
}

/// A refusal of one of a project's limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// The limit's key, e.g. `limits.cpu`.
    pub key: String,
    /// Its value as incus quoted it, when it did.
    pub value: Option<String>,
    pub project: String,
}

/// The project-limit refusal in an incus error message, if it is one.
pub(crate) fn parse(msg: &str) -> Option<Refusal> {
    let at = msg.find("Reached maximum ")?;
    let rest = &msg[at..];
    let quoted: Vec<&str> = rest.split('"').skip(1).step_by(2).collect();
    if rest.starts_with("Reached maximum aggregate value ") {
        let [value, key, project] = quoted[..] else {
            return None;
        };
        return Some(Refusal {
            key: key.to_string(),
            value: Some(value.to_string()),
            project: project.to_string(),
        });
    }
    if rest.starts_with("Reached maximum number of instances of type ") {
        let [kind, project] = quoted[..] else {
            return None;
        };
        let key = if kind.starts_with("virtual") {
            "limits.virtual-machines"
        } else {
            "limits.containers"
        };
        return Some(Refusal {
            key: key.into(),
            value: None,
            project: project.to_string(),
        });
    }
    if rest.starts_with("Reached maximum number of instances in project ") {
        let [project] = quoted[..] else { return None };
        return Some(Refusal {
            key: "limits.instances".into(),
            value: None,
            project: project.to_string(),
        });
    }
    None
}

/// The project's limit and allocation of what `key` limits, from
/// `/1.0/projects/<p>/state` (`resources.<name>.{Limit,Usage}`; incus'
/// "usage" is the sum of the instances' configured limits).
pub(crate) fn usage(state: &Value, key: &str) -> Option<(i64, i64)> {
    let name = key.strip_prefix("limits.")?;
    let r = &state["resources"][name];
    let get = |k: &str, alt: &str| r[k].as_i64().or_else(|| r[alt].as_i64());
    Some((get("Limit", "limit")?, get("Usage", "usage")?))
}

/// Bytes as incus sizes are written: `3.5GiB`, `512MiB`.
pub fn bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v.fract() == 0.0 {
        format!("{v}{}", UNITS[u])
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

/// What the limit counts, and `isb org update`'s flag that sets it.
fn describe(key: &str) -> (&'static str, Option<&'static str>) {
    match key {
        "limits.cpu" => ("CPU", Some("--cpus")),
        "limits.memory" => ("memory", Some("--memory")),
        k if k.starts_with("limits.disk") => ("disk", Some("--disk")),
        "limits.instances" => ("instance", Some("--instances")),
        "limits.containers" => ("container", None),
        "limits.virtual-machines" => ("VM", None),
        _ => ("resource", None),
    }
}

/// What the refused request asked for: the instance (or volume) and its
/// amount of the limited resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Need {
    pub name: Option<String>,
    pub amount: String,
}

/// What a refused create or update of an instance or volume (`path`,
/// `body`) asked for of `key`: the body's own value, else the org's
/// default profile's (what the instance would have got).
fn needed(c: &Client, key: &str, path: &str, body: &Value) -> Option<Need> {
    let tail = path.split('?').next().unwrap_or(path);
    let name = body["name"]
        .as_str()
        .map(String::from)
        .or_else(|| tail.rsplit('/').next().map(String::from))
        .filter(|n| !n.is_empty() && n != "instances" && n != "volumes");
    let disk = key.starts_with("limits.disk");
    let amount = if tail.contains("/volumes") {
        disk.then(|| body["config"]["size"].as_str().map(String::from))??
    } else if tail.starts_with("/1.0/instances") {
        let own = if disk {
            body["devices"]["root"]["size"].as_str()
        } else {
            body["config"][key].as_str()
        };
        match own {
            Some(v) => v.to_string(),
            None if key == "limits.cpu" || key == "limits.memory" || disk => {
                let p = c.get("/1.0/profiles/default").ok()?;
                let v = if disk {
                    &p["devices"]["root"]["size"]
                } else {
                    &p["config"][key]
                };
                format!("{} (the org's default)", v.as_str()?)
            }
            None => return None,
        }
    } else {
        return None;
    };
    Some(Need { name, amount })
}

/// The message for a refusal, given the project's limit and allocation when
/// they could be read, and what the request needed when it is known.
pub(crate) fn explain(r: &Refusal, used: Option<(i64, i64)>, need: Option<&Need>) -> String {
    let (what, flag) = describe(&r.key);
    let in_bytes = matches!(what, "memory" | "disk");
    let fmt = |n: i64| if in_bytes { bytes(n) } else { n.to_string() };
    let counted = matches!(what, "instance" | "container" | "VM");
    let state = match used {
        Some((l, u)) if counted => format!("{}: {u} of {l}, stopped ones included", r.key),
        Some((l, u)) => format!(
            "{}: allocated {} of {}, the sum of every instance's limit, stopped ones included",
            r.key,
            fmt(u),
            fmt(l)
        ),
        None => format!("{} {}", r.key, r.value.as_deref().unwrap_or("?")),
    };
    let need = need
        .map(|n| match &n.name {
            Some(name) => format!("; {name} needs {}", n.amount),
            None => format!("; the request needs {}", n.amount),
        })
        .unwrap_or_default();
    let free = if counted {
        "delete one, or ask for fewer"
    } else {
        "delete an instance or lower one's limit (stopping one frees nothing), or ask for less"
    };
    match OrgId::from_incus_project(&r.project) {
        Some(org) => {
            let raise = match flag {
                Some(f) => format!(
                    "a platform admin raises it with `isb org update {org} {f} N` on the host, or lifts it with `{f} none` (or the org_update tool)"
                ),
                None => format!(
                    "isb does not set {}; an operator raises it with `incus project set {} {}=N`",
                    r.key, r.project, r.key
                ),
            };
            format!("org {org} is at its {what} quota ({state}{need}): {free}, or {raise}")
        }
        None => format!(
            "incus project {} is at its {what} limit ({state}{need}): {free}, or raise it with `incus project set {} {}=...`",
            r.project, r.project, r.key
        ),
    }
}

/// `message` from incusd, as the error to return: a clear quota error when
/// it is a project-limit refusal (with the allocation `c` can read and what
/// `request`, the refused path and body, asked for), else `None`.
pub(crate) fn translate(
    c: &Client,
    message: &str,
    request: Option<(&str, &Value)>,
) -> Option<Error> {
    let r = parse(message)?;
    let state = c.clone().project("").get(&format!(
        "/1.0/projects/{}/state",
        crate::client::encode_segment(&r.project)
    ));
    let used = state.ok().and_then(|s| usage(&s, &r.key));
    let need = request.and_then(|(path, body)| needed(c, &r.key, path, body));
    Some(Error::Invalid(explain(&r, used, need.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_client_turns_a_quota_refusal_into_a_clear_error() {
        use crate::client::fake::{Route, serve};
        let (_d, c) = serve(vec![
            Route {
                prefix: "POST /1.0/instances",
                status: 500,
                body: json!(
                    r#"Failed checking if instance creation allowed: Reached maximum aggregate value "2" for "limits.cpu" in project "isb-lab""#
                ),
            },
            Route {
                prefix: "GET /1.0/projects/isb-lab/state",
                status: 200,
                body: json!({"resources": {"cpu": {"Limit": 2, "Usage": 2}}}),
            },
            Route {
                prefix: "GET /1.0/profiles/default",
                status: 200,
                body: json!({"config": {"limits.cpu": "1"}, "devices": {}}),
            },
        ]);
        let c = c.project("isb-lab");
        let e = c
            .mutate(
                "POST",
                "/1.0/instances",
                Some(&json!({"name": "web-1", "config": {}})),
                "create",
                c.timeouts.other,
            )
            .unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(
            e.to_string().starts_with(
                "org lab is at its CPU quota (limits.cpu: allocated 2 of 2, the sum of every instance's limit, stopped ones included; web-1 needs 1 (the org's default))"
            ),
            "{e}"
        );
        // Any other incus error is passed on as it was.
        let e = c.get("/1.0/instances/x").unwrap_err();
        assert!(e.is_not_found(), "{e}");
    }

    #[test]
    fn parses_incus_refusals() {
        let m = r#"Failed checking if instance creation allowed: Reached maximum aggregate value "2" for "limits.cpu" in project "isb-lab""#;
        assert_eq!(
            parse(m),
            Some(Refusal {
                key: "limits.cpu".into(),
                value: Some("2".into()),
                project: "isb-lab".into()
            })
        );
        let m = r#"Failed checking if instance creation allowed: Reached maximum number of instances of type "container" in project "isb-lab""#;
        assert_eq!(parse(m).unwrap().key, "limits.containers");
        let m = r#"Reached maximum number of instances in project "isb-lab""#;
        let r = parse(m).unwrap();
        assert_eq!((r.key.as_str(), r.value), ("limits.instances", None));
        assert_eq!(parse("Project not found"), None);
        assert_eq!(parse(r#"Reached maximum aggregate value "2""#), None);
    }

    #[test]
    fn reads_usage_from_project_state() {
        let s = json!({"resources": {"cpu": {"Limit": 2, "Usage": 2}, "memory": {"limit": 4, "usage": 3}}});
        assert_eq!(usage(&s, "limits.cpu"), Some((2, 2)));
        assert_eq!(usage(&s, "limits.memory"), Some((4, 3)));
        assert_eq!(usage(&s, "limits.disk"), None);
    }

    #[test]
    fn budgets_are_the_limited_resources_with_what_is_left() {
        let s = json!({"resources": {
            "cpu": {"Limit": 4, "Usage": 3},
            "memory": {"Limit": 4i64 << 30, "Usage": 5i64 << 30},
            "disk": {"Limit": -1, "Usage": 0},
            "instances": {"Limit": 5, "Usage": 3},
            "networks": {"Limit": 2, "Usage": 0},
        }});
        let b = budgets(&s);
        assert_eq!(b.keys().collect::<Vec<_>>(), ["cpu", "instances", "memory"]);
        assert_eq!(
            b["cpu"],
            Budget {
                limit: 4,
                allocated: 3,
                free: 1
            }
        );
        assert_eq!(b["cpu"].text(false), "3 of 4 allocated, 1 free");
        // Over budget (the limit was lowered under what is allocated): none free.
        assert_eq!(b["memory"].free, 0);
        assert_eq!(b["memory"].text(true), "5GiB of 4GiB allocated, 0B free");
    }

    #[test]
    fn a_refused_request_says_what_it_needed() {
        let (_d, c) = crate::client::fake::serve(vec![]);
        let body = json!({"name": "db-1", "config": {"limits.memory": "2GiB"}, "devices": {"root": {"size": "20GiB"}}});
        let n = needed(&c, "limits.memory", "/1.0/instances", &body).unwrap();
        assert_eq!(
            (n.name.as_deref(), n.amount.as_str()),
            (Some("db-1"), "2GiB")
        );
        let n = needed(&c, "limits.disk", "/1.0/instances", &body).unwrap();
        assert_eq!(n.amount, "20GiB");
        // A resize names the instance from the path.
        let n = needed(
            &c,
            "limits.cpu",
            "/1.0/instances/web-2",
            &json!({"config": {"limits.cpu": "4"}}),
        )
        .unwrap();
        assert_eq!((n.name.as_deref(), n.amount.as_str()), (Some("web-2"), "4"));
        let v = json!({"name": "data", "config": {"size": "5GiB"}});
        let n = needed(&c, "limits.disk", "/1.0/storage-pools/p/volumes/custom", &v).unwrap();
        assert_eq!(
            (n.name.as_deref(), n.amount.as_str()),
            (Some("data"), "5GiB")
        );
        // Counts, and requests that are not about an instance or a volume: nothing.
        assert_eq!(
            needed(&c, "limits.instances", "/1.0/instances", &body),
            None
        );
        assert_eq!(
            needed(&c, "limits.cpu", "/1.0/projects/isb-lab", &body),
            None
        );
    }

    #[test]
    fn explains_a_full_quota_with_its_allocation_and_the_way_to_raise_it() {
        let r = Refusal {
            key: "limits.cpu".into(),
            value: Some("2".into()),
            project: "isb-lab".into(),
        };
        let m = explain(&r, Some((2, 2)), None);
        assert!(
            m.starts_with("org lab is at its CPU quota (limits.cpu: allocated 2 of 2, the sum of every instance's limit, stopped ones included):"),
            "{m}"
        );
        assert!(m.contains("stopping one frees nothing"), "{m}");
        assert!(m.contains("`isb org update lab --cpus N`"), "{m}");
        assert!(m.contains("`--cpus none`"), "{m}");
        assert!(m.contains("org_update"), "{m}");

        let r = Refusal {
            key: "limits.memory".into(),
            value: None,
            project: "isb-lab".into(),
        };
        let need = Need {
            name: Some("db-1".into()),
            amount: "1GiB".into(),
        };
        let m = explain(&r, Some((4 << 30, 3584 << 20)), Some(&need));
        assert!(
            m.contains("(limits.memory: allocated 3.5GiB of 4GiB, the sum of every instance's limit, stopped ones included; db-1 needs 1GiB)"),
            "{m}"
        );
        assert!(m.contains("--memory N"), "{m}");

        let r = Refusal {
            key: "limits.instances".into(),
            value: None,
            project: "isb-lab".into(),
        };
        let m = explain(&r, Some((3, 3)), None);
        assert!(
            m.contains("(limits.instances: 3 of 3, stopped ones included): delete one"),
            "{m}"
        );

        let r = Refusal {
            key: "limits.containers".into(),
            value: None,
            project: "isb-lab".into(),
        };
        let m = explain(&r, None, None);
        assert!(m.contains("(limits.containers ?)"), "{m}");
        assert!(
            m.contains("incus project set isb-lab limits.containers=N"),
            "{m}"
        );

        let r = Refusal {
            key: "limits.cpu".into(),
            value: Some("8".into()),
            project: "someone-else".into(),
        };
        let m = explain(&r, None, None);
        assert!(
            m.starts_with("incus project someone-else is at its CPU limit (limits.cpu 8)"),
            "{m}"
        );
    }
}
