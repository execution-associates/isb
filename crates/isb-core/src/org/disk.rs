//! Root disk sizes under an org's disk limit (`limits.disk`).
//!
//! incus counts every root disk's `size` against the limit, refuses to
//! create an instance whose root disk has none, and refuses to set the limit
//! while one has none. isb sizes each instance it creates there itself, in
//! the create request: the spec's `raw_devices.root.size`, else
//! [`DEFAULT_ROOT_SIZE`]. It never writes a size on the org's default
//! profile: incus applies a profile's size to every instance that takes its
//! root disk from the profile, so one there would resize them all.

use serde_json::{Value, json};

use super::OrgId;
use super::limits::{DEFAULT_ROOT_SIZE, bytes};
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};

/// Whether `c`'s project has `limits.disk`. An unreadable project counts as
/// none: incus itself refuses what the limit forbids.
pub fn disk_limited(c: &Client) -> bool {
    let path = format!("/1.0/projects/{}", encode_segment(c.project_name()));
    c.clone()
        .project("default")
        .get_opt(&path)
        .ok()
        .flatten()
        .is_some_and(|p| p["config"]["limits.disk"].is_string())
}

/// An instance create `body` for `c`'s project: a root disk of its own
/// without a size gets [`DEFAULT_ROOT_SIZE`] while the project has a disk
/// limit. Only the request changes, never the spec it came from (a stack's
/// revision hashes the spec).
pub fn sized_root(c: &Client, mut body: Value) -> Value {
    let root = &body["devices"]["root"];
    if root.is_object() && root.get("size").is_none() && disk_limited(c) {
        body["devices"]["root"]["size"] = json!(DEFAULT_ROOT_SIZE);
    }
    body
}

/// An instance whose root disk has no size, and what that disk holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsized {
    pub name: String,
    pub usage: Option<i64>,
}

/// The instances of `oc`'s project whose root disk (their own, else their
/// profiles') has no size: the ones that block a disk limit.
pub(crate) fn unsized_roots(oc: &Client) -> Result<Vec<Unsized>> {
    let list = oc.get("/1.0/instances?recursion=1")?;
    let names: Vec<String> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["expanded_devices"]["root"].get("size").is_none())
        .filter_map(|i| i["name"].as_str().map(String::from))
        .collect();
    // What each holds, read for the first few only: it is advice.
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(n, name)| {
            let usage = (n < 20)
                .then(|| {
                    oc.get_opt(&format!("/1.0/instances/{}/state", encode_segment(&name)))
                        .ok()
                        .flatten()
                })
                .flatten()
                .and_then(|s| s["disk"]["root"]["usage"].as_i64());
            Unsized { name, usage }
        })
        .collect())
}

/// The refusal to set a disk limit on `org` while `list` have no root
/// size: incus would refuse it too, saying less.
pub(crate) fn refuse_unsized(org: &OrgId, list: &[Unsized]) -> Error {
    let each: Vec<String> = list
        .iter()
        .map(|u| match u.usage {
            Some(b) => format!("{} (holds {})", u.name, bytes(b)),
            None => u.name.clone(),
        })
        .collect();
    Error::invalid(format!(
        "org {org}: a disk limit needs every instance's root disk to have a size, and {} {} none: {}. \
         Give each one's service `raw_devices: {{root: {{size: ...}}}}` (at least what it holds) \
         and redeploy it, or delete the instance; then set the limit again",
        each.len(),
        if each.len() == 1 { "has" } else { "have" },
        each.join(", ")
    ))
}

/// The instances of `oc`'s project that take their root disk from the
/// default profile (they have none of their own), so a size there is theirs.
pub(crate) fn on_profile_root(oc: &Client) -> Result<Vec<String>> {
    let list = oc.get("/1.0/instances?recursion=1")?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| {
            i["devices"].get("root").is_none()
                && i["profiles"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|p| p == "default"))
        })
        .filter_map(|i| i["name"].as_str().map(String::from))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fake::{Route, serve};

    fn project(limited: bool) -> Route {
        let config = if limited {
            json!({"limits.disk": "100GiB"})
        } else {
            json!({})
        };
        Route {
            prefix: "GET /1.0/projects/isb-lab",
            status: 200,
            body: json!({"name": "isb-lab", "config": config}),
        }
    }

    fn body(root: Value) -> Value {
        json!({"name": "web-1", "config": {"user.isb.rev": "d59025b7"}, "devices": {"root": root}})
    }

    #[test]
    fn a_new_root_disk_is_sized_only_under_a_disk_limit() {
        let root = json!({"type": "disk", "path": "/", "pool": "default"});
        let (_d, c) = serve(vec![project(false)]);
        let c = c.project("isb-lab");
        let b = sized_root(&c, body(root.clone()));
        assert!(b["devices"]["root"].get("size").is_none(), "{b}");

        let (_d, c) = serve(vec![project(true)]);
        let c = c.project("isb-lab");
        let b = sized_root(&c, body(root.clone()));
        assert_eq!(b["devices"]["root"]["size"], "10GiB");
        // The rest of the request (the revision label with it) is as it was.
        assert_eq!(b["config"], body(root.clone())["config"]);
        assert_eq!(b["devices"]["root"]["pool"], "default");

        // The service's own size wins.
        let mut own = root.clone();
        own["size"] = json!("30GiB");
        let b = sized_root(&c, body(own));
        assert_eq!(b["devices"]["root"]["size"], "30GiB");

        // No root of its own: left to its profile (and to incus).
        let b = sized_root(&c, json!({"name": "x", "devices": {}}));
        assert!(b["devices"].get("root").is_none(), "{b}");
    }

    #[test]
    fn an_unreadable_project_has_no_disk_limit() {
        let (_d, c) = serve(vec![]);
        assert!(!disk_limited(&c.project("isb-lab")));
    }

    #[test]
    fn instances_without_a_root_size_are_named_with_what_they_hold() {
        let (_d, c) = serve(vec![
            Route {
                prefix: "GET /1.0/instances?recursion=1",
                status: 200,
                body: json!([
                    {"name": "sized", "profiles": ["default"], "devices": {"root": {"size": "10GiB"}},
                     "expanded_devices": {"root": {"size": "10GiB"}}},
                    {"name": "by-profile", "profiles": ["default"], "devices": {},
                     "expanded_devices": {"root": {"size": "10GiB"}}},
                    {"name": "web-1", "profiles": ["default"], "devices": {"root": {"path": "/"}},
                     "expanded_devices": {"root": {"path": "/"}}},
                    {"name": "db-1", "profiles": ["default"], "devices": {},
                     "expanded_devices": {"root": {"path": "/"}}},
                ]),
            },
            Route {
                prefix: "GET /1.0/instances/web-1/state",
                status: 200,
                body: json!({"disk": {"root": {"usage": 3_221_225_472_i64}}}),
            },
        ]);
        let c = c.project("isb-lab");
        let u = unsized_roots(&c).unwrap();
        assert_eq!(
            u,
            vec![
                Unsized {
                    name: "web-1".into(),
                    usage: Some(3_221_225_472)
                },
                Unsized {
                    name: "db-1".into(),
                    usage: None
                },
            ]
        );
        let e = refuse_unsized(&OrgId::new("lab").unwrap(), &u).to_string();
        assert!(e.contains("2 have none: web-1 (holds 3GiB), db-1."), "{e}");
        assert!(e.contains("raw_devices: {root: {size: ...}}"), "{e}");
        assert_eq!(on_profile_root(&c).unwrap(), vec!["by-profile", "db-1"]);
    }
}
