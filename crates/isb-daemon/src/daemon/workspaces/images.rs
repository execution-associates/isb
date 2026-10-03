//! What a new workspace can be made from, and how much of the org's quota
//! is left for it: the image choices and the default `workspace_create`
//! uses when the call names none, and the org's headroom, for the create
//! form to show before anything is submitted.

use super::*;
use crate::sandbox::images::{self, LocalImage, REMOTE_DEFAULT};

/// The image a workspace gets when the call names none: the local
/// `dev-base` when this host has it, else a remote image every host can
/// pull.
pub(super) const PREFERRED_LOCAL: &str = "dev-base";

pub(super) fn default_image(local: &[LocalImage]) -> &'static str {
    if local.iter().any(|i| i.alias == PREFERRED_LOCAL) {
        PREFERRED_LOCAL
    } else {
        REMOTE_DEFAULT
    }
}

/// [`default_image`] for the org's project (the remote default when its
/// images cannot be read).
pub(super) fn default_for(oc: &Client) -> String {
    default_image(&images::local(oc).unwrap_or_default()).to_string()
}

/// The choices: each local image, then the remote default.
fn choices(local: &[LocalImage]) -> Vec<Value> {
    let mut out: Vec<Value> = local
        .iter()
        .map(|i| json!({"image": i.alias, "description": i.description, "source": "local"}))
        .collect();
    out.push(json!({
        "image": REMOTE_DEFAULT,
        "description": "Ubuntu 24.04 from images.linuxcontainers.org: pulled on first use, works on any host",
        "source": "remote",
    }));
    out
}

/// The org's quota and what is in use, from its project's state:
/// `{cpu, memory, disk, instances}`, each `{limit, usage}` with a `null`
/// limit when there is none (memory and disk in bytes).
pub(super) fn quota(state: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ["cpu", "memory", "disk", "instances"] {
        let r = &state["resources"][k];
        let get = |a: &str, b: &str| r[a].as_i64().or_else(|| r[b].as_i64());
        let (Some(limit), Some(usage)) = (get("Limit", "limit"), get("Usage", "usage")) else {
            continue;
        };
        out.insert(
            k.into(),
            json!({"limit": (limit >= 0).then_some(limit), "usage": usage}),
        );
    }
    Value::Object(out)
}

/// What the create form offers: the images, the default, and the quota.
pub(super) fn create_options(wsm: &Workspaces, org: &OrgId) -> Value {
    let oc = wsm.oc(org);
    let local = images::local(&oc).unwrap_or_default();
    let state = wsm
        .client
        .get(&format!(
            "/1.0/projects/{}/state",
            encode_segment(&org.incus_project())
        ))
        .unwrap_or_default();
    json!({
        "images": choices(&local),
        "default_image": default_image(&local),
        "quota": quota(&state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(a: &str) -> LocalImage {
        LocalImage {
            alias: a.into(),
            description: String::new(),
        }
    }

    #[test]
    fn the_default_image_is_dev_base_only_where_it_exists() {
        assert_eq!(default_image(&[img("alpine"), img("dev-base")]), "dev-base");
        assert_eq!(default_image(&[img("alpine")]), "images:ubuntu/24.04");
        assert_eq!(default_image(&[]), "images:ubuntu/24.04");
        let c = choices(&[img("alpine")]);
        assert_eq!(c[0]["source"], "local");
        assert_eq!(c[1]["image"], "images:ubuntu/24.04");
    }

    #[test]
    fn quota_headroom_from_the_project_state() {
        let s = json!({"resources": {
            "cpu": {"Limit": 2, "Usage": 2},
            "memory": {"Limit": -1, "Usage": 1024},
            "networks": {"Limit": -1, "Usage": 0}
        }});
        assert_eq!(
            quota(&s),
            json!({"cpu": {"limit": 2, "usage": 2}, "memory": {"limit": null, "usage": 1024}})
        );
        assert_eq!(quota(&Value::Null), json!({}));
    }
}
