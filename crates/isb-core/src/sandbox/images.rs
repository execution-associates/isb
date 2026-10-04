//! The images a project can create instances from without a remote, and
//! what to say when a spec names one that is not there.

use serde_json::Value;

use crate::client::Client;
use crate::error::Error;

/// A remote image that every host can pull: what to suggest when a local
/// one is missing.
pub const REMOTE_DEFAULT: &str = "images:ubuntu/24.04";

/// A local image by alias, with its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    pub alias: String,
    pub description: String,
}

/// The aliases of `/1.0/images?recursion=1`, sorted (an image with two
/// aliases is listed twice; one without any is left out).
pub fn aliases(images: &Value) -> Vec<LocalImage> {
    let mut out: Vec<LocalImage> = images
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|i| {
            let description = i["properties"]["description"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            i["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["name"].as_str())
                .map(move |a| LocalImage {
                    alias: a.to_string(),
                    description: description.clone(),
                })
        })
        .collect();
    out.sort_by(|a, b| a.alias.cmp(&b.alias));
    out
}

/// The images the client's project can use by alias.
pub fn local(client: &Client) -> crate::error::Result<Vec<LocalImage>> {
    Ok(aliases(&client.get("/1.0/images?recursion=1")?))
}

/// The error for a local image that is not there: which ones are, and how
/// to name a remote one instead.
pub(crate) fn missing(alias: &str, have: &[LocalImage]) -> Error {
    const SHOWN: usize = 12;
    let list = if have.is_empty() {
        "this host has no local images".to_string()
    } else {
        let names: Vec<&str> = have.iter().take(SHOWN).map(|i| i.alias.as_str()).collect();
        let more = have.len().saturating_sub(SHOWN);
        format!(
            "local images: {}{}",
            names.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        )
    };
    Error::invalid(format!(
        "image {alias:?} not found locally ({list}); name a remote image with its server instead, e.g. {REMOTE_DEFAULT}"
    ))
}

/// [`missing`] with the local images read from `client` (none if they
/// cannot be read).
pub(crate) fn missing_on(client: &Client, alias: &str) -> Error {
    missing(alias, &local(client).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_aliases_and_says_how_to_pick_a_remote_image() {
        let v = json!([
            {"aliases": [{"name": "dev-base"}], "properties": {"description": "Ubuntu for dev"}},
            {"aliases": [], "properties": {}},
            {"aliases": [{"name": "alpine"}, {"name": "a2"}]}
        ]);
        let have = aliases(&v);
        let names: Vec<&str> = have.iter().map(|i| i.alias.as_str()).collect();
        assert_eq!(names, ["a2", "alpine", "dev-base"]);
        assert_eq!(have[2].description, "Ubuntu for dev");

        let m = missing("dev-base", &have[..2]).to_string();
        assert!(
            m.starts_with("image \"dev-base\" not found locally (local images: a2, alpine)"),
            "{m}"
        );
        assert!(m.contains("e.g. images:ubuntu/24.04"), "{m}");
        let m = missing("dev-base", &[]).to_string();
        assert!(m.contains("(this host has no local images)"), "{m}");
    }
}
