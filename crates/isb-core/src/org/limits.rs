//! incus' project-limit refusals, said in isb's terms: which of the org's
//! quotas is full, how much of it is in use, and how to raise it.
//!
//! incus checks a project's `limits.*` when an instance or a volume is
//! created or resized, and refuses with text such as `Reached maximum
//! aggregate value "2" for "limits.cpu" in project "isb-lab"`. The client
//! turns any such answer into [`Error::Invalid`] with [`explain`]'s message,
//! so every path that creates instances in an org (sandboxes, workspaces,
//! apps, databases, builds) says the same thing.

use serde_json::Value;

use super::OrgId;
use crate::client::Client;
use crate::error::Error;

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

/// The project's limit and usage of what `key` limits, from
/// `/1.0/projects/<p>/state` (`resources.<name>.{Limit,Usage}`).
pub(crate) fn usage(state: &Value, key: &str) -> Option<(i64, i64)> {
    let name = key.strip_prefix("limits.")?;
    let r = &state["resources"][name];
    let get = |k: &str, alt: &str| r[k].as_i64().or_else(|| r[alt].as_i64());
    Some((get("Limit", "limit")?, get("Usage", "usage")?))
}

/// Bytes as incus sizes are written: `3.5GiB`, `512MiB`.
fn bytes(n: i64) -> String {
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

/// What the limit counts, and `isb org create`'s flag that sets it.
fn describe(key: &str) -> (&'static str, Option<&'static str>) {
    match key {
        "limits.cpu" => ("CPU", Some("--cpus N")),
        "limits.memory" => ("memory", Some("--memory SIZE")),
        k if k.starts_with("limits.disk") => ("disk", Some("--disk SIZE")),
        "limits.instances" => ("instance", Some("--instances N")),
        "limits.containers" => ("container", None),
        "limits.virtual-machines" => ("VM", None),
        _ => ("resource", None),
    }
}

/// The message for a refusal, given the project's limit and usage when
/// they could be read.
pub(crate) fn explain(r: &Refusal, used: Option<(i64, i64)>) -> String {
    let (what, flag) = describe(&r.key);
    let in_bytes = matches!(what, "memory" | "disk");
    let fmt = |n: i64| if in_bytes { bytes(n) } else { n.to_string() };
    let limit = r
        .value
        .clone()
        .or_else(|| used.map(|(l, _)| fmt(l)))
        .unwrap_or_else(|| "?".into());
    let in_use = used
        .map(|(_, u)| format!(", {} in use", fmt(u)))
        .unwrap_or_default();
    let free = "stop or delete something in it, or ask for less";
    match OrgId::from_incus_project(&r.project) {
        Some(org) => {
            let raise = match flag {
                Some(f) => format!(
                    "a platform admin raises it with `isb org create {org} {f}` on the host or the org_update tool"
                ),
                None => format!(
                    "isb does not set {}; an operator raises it with `incus project set {} {}=N`",
                    r.key, r.project, r.key
                ),
            };
            format!(
                "org {org} is at its {what} quota ({} {limit}{in_use}): {free}, or {raise}",
                r.key
            )
        }
        None => format!(
            "incus project {} is at its {what} limit ({} {limit}{in_use}): {free}, or raise it with `incus project set {} {}=...`",
            r.project, r.key, r.project, r.key
        ),
    }
}

/// `message` from incusd, as the error to return: a clear quota error when
/// it is a project-limit refusal (with the usage `c` can read), else `None`.
pub(crate) fn translate(c: &Client, message: &str) -> Option<Error> {
    let r = parse(message)?;
    let state = c.clone().project("").get(&format!(
        "/1.0/projects/{}/state",
        crate::client::encode_segment(&r.project)
    ));
    let used = state.ok().and_then(|s| usage(&s, &r.key));
    Some(Error::Invalid(explain(&r, used)))
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
        ]);
        let c = c.project("isb-lab");
        let e = c
            .mutate(
                "POST",
                "/1.0/instances",
                Some(&json!({})),
                "create",
                c.timeouts.other,
            )
            .unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(
            e.to_string()
                .starts_with("org lab is at its CPU quota (limits.cpu 2, 2 in use)"),
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
    fn explains_a_full_quota_with_its_usage_and_the_way_to_raise_it() {
        let r = Refusal {
            key: "limits.cpu".into(),
            value: Some("2".into()),
            project: "isb-lab".into(),
        };
        let m = explain(&r, Some((2, 2)));
        assert!(
            m.starts_with("org lab is at its CPU quota (limits.cpu 2, 2 in use)"),
            "{m}"
        );
        assert!(m.contains("`isb org create lab --cpus N`"), "{m}");
        assert!(m.contains("org_update"), "{m}");

        let r = Refusal {
            key: "limits.memory".into(),
            value: None,
            project: "isb-lab".into(),
        };
        let m = explain(&r, Some((4 << 30, 3584 << 20)));
        assert!(m.contains("(limits.memory 4GiB, 3.5GiB in use)"), "{m}");
        assert!(m.contains("--memory SIZE"), "{m}");

        let r = Refusal {
            key: "limits.containers".into(),
            value: None,
            project: "isb-lab".into(),
        };
        let m = explain(&r, None);
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
        let m = explain(&r, None);
        assert!(
            m.starts_with("incus project someone-else is at its CPU limit"),
            "{m}"
        );
    }
}
