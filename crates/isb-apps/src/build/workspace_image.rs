//! Workspace images from recipe scripts (docs/guides/workspace-images.md).
//!
//! A recipe is a shell script run as root in a throwaway container made
//! from a base image (`images:ubuntu/24.04` unless told otherwise). The
//! container lives in the `isb-system` project, never in an org, on the
//! host's default bridge; when the recipe succeeds it is stopped and
//! published as a local image under the asked alias, then deleted. However
//! the build ends, the container goes, and a failed build publishes
//! nothing.
//!
//! The images isb builds carry `isb.workspace-image=1` among their
//! properties, with the recipe's SHA-256, the base, who built it and when.
//! Only such images can be replaced or removed through isb: an alias that
//! names any other image (`dev-base`, say) is refused. Building again with
//! the same recipe and base is a no-op unless forced.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Remove, ready, remaining, stream_lines, uplink_network};
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::exec::ExecOptions;
use crate::sandbox::Sandbox;

/// isb's default workspace recipe.
pub const DEFAULT_RECIPE: &str = include_str!("workspace-image.sh");
/// The alias the default recipe is published under.
pub const DEFAULT_NAME: &str = "isb-workspace";
/// The base a recipe runs on unless told otherwise.
pub const DEFAULT_BASE: &str = "images:ubuntu/24.04";
/// How long a recipe may run, by default and at most.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(2 * 3600);
/// The largest recipe accepted.
pub const MAX_RECIPE: usize = 256 * 1024;

/// Image properties isb sets on the images it builds.
pub const PROP_MARK: &str = "isb.workspace-image";
pub const PROP_RECIPE: &str = "isb.recipe-sha256";
pub const PROP_BASE: &str = "isb.base";
pub const PROP_BUILT_BY: &str = "isb.built-by";
pub const PROP_BUILT_AT: &str = "isb.built-at";

/// Where the recipe is pushed in the build container.
const RECIPE_PATH: &str = "/root/isb-recipe.sh";

/// What to build.
#[derive(Debug, Clone)]
pub struct ImageBuild {
    /// The local image alias to publish.
    pub name: String,
    pub recipe: String,
    pub base: String,
    pub description: Option<String>,
    pub timeout: Duration,
    /// Build even when the image is up to date.
    pub force: bool,
    pub built_by: String,
}

impl ImageBuild {
    /// The default recipe under its own name.
    pub fn default_recipe(built_by: &str) -> ImageBuild {
        ImageBuild {
            name: DEFAULT_NAME.into(),
            recipe: DEFAULT_RECIPE.into(),
            base: DEFAULT_BASE.into(),
            description: None,
            timeout: DEFAULT_TIMEOUT,
            force: false,
            built_by: built_by.into(),
        }
    }

    pub fn recipe_sha256(&self) -> String {
        recipe_sha256(&self.recipe)
    }

    /// The description the image gets: the one asked for, else one naming
    /// the recipe.
    pub fn description(&self) -> String {
        match &self.description {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ if self.recipe == DEFAULT_RECIPE => format!(
                "isb workspace: Ubuntu 24.04, dev (uid 1000), mise (node, bun, uv), Claude Code, omp, herdr (built {})",
                today()
            ),
            _ => format!(
                "workspace image from a recipe on {} (built {})",
                self.base,
                today()
            ),
        }
    }
}

pub fn recipe_sha256(recipe: &str) -> String {
    let d = crate::registry::oci::digest_of(recipe.as_bytes());
    d.trim_start_matches("sha256:").to_string()
}

fn today() -> String {
    let t = crate::stack::now_secs() as i64;
    crate::cron::rfc3339(t)[..10].to_string()
}

/// An image alias isb may publish: a lower-case name of letters, digits,
/// `.`, `_` and `-`, at most 63 long, not one of incus' remote forms.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "image name {name:?}: lower-case letters, digits, '.', '_' and '-', starting with a letter or digit, at most 63"
        )))
    }
}

/// Check a build request before anything is made.
pub fn check(b: &ImageBuild) -> Result<()> {
    check_name(&b.name)?;
    if b.recipe.trim().is_empty() {
        return Err(Error::invalid("the recipe is empty"));
    }
    if b.recipe.len() > MAX_RECIPE {
        return Err(Error::invalid(format!(
            "the recipe is {} bytes; at most {MAX_RECIPE}",
            b.recipe.len()
        )));
    }
    if b.base.trim().is_empty() {
        return Err(Error::invalid("base cannot be empty"));
    }
    crate::plan::ImageSource::parse(&b.base)?;
    if b.timeout.is_zero() || b.timeout > MAX_TIMEOUT {
        return Err(Error::invalid(format!(
            "timeout: more than 0 and at most {} minutes",
            MAX_TIMEOUT.as_secs() / 60
        )));
    }
    Ok(())
}

/// An image an alias names now, as far as a build cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub fingerprint: String,
    /// Built by isb from a recipe (`isb.workspace-image`).
    pub ours: bool,
    pub recipe_sha256: Option<String>,
    pub base: Option<String>,
    /// Every alias of the image (the one asked about included).
    pub aliases: Vec<String>,
}

impl Existing {
    pub fn from_image(v: &Value) -> Existing {
        let p = &v["properties"];
        let prop = |k: &str| p[k].as_str().map(str::to_string);
        Existing {
            fingerprint: v["fingerprint"].as_str().unwrap_or_default().to_string(),
            ours: p[PROP_MARK].as_str() == Some("1"),
            recipe_sha256: prop(PROP_RECIPE),
            base: prop(PROP_BASE),
            aliases: v["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["name"].as_str().map(str::to_string))
                .collect(),
        }
    }
}

/// What a build does about the alias it was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing there yet: build and add the alias.
    Create,
    /// isb's image from the same recipe and base: nothing to do.
    UpToDate { fingerprint: String },
    /// isb's image from another recipe or base (or forced): build, move the
    /// alias, and delete the old image if nothing else names it.
    Replace { old: String },
}

/// Decide what a build does, refusing an alias isb did not make.
pub fn plan(existing: Option<&Existing>, b: &ImageBuild) -> Result<Plan> {
    let Some(e) = existing else {
        return Ok(Plan::Create);
    };
    if !e.ours {
        return Err(Error::AlreadyExists(format!(
            "image {} exists and was not built by isb from a recipe; pick another name (isb replaces only the images it built)",
            b.name
        )));
    }
    let same = e.recipe_sha256.as_deref() == Some(b.recipe_sha256().as_str())
        && e.base.as_deref() == Some(b.base.as_str());
    if same && !b.force {
        return Ok(Plan::UpToDate {
            fingerprint: e.fingerprint.clone(),
        });
    }
    Ok(Plan::Replace {
        old: e.fingerprint.clone(),
    })
}

/// Whether `isb workspace image rm` may remove this image: only isb's.
pub fn removable(name: &str, e: &Existing) -> Result<()> {
    if e.ours {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "image {name} was not built by isb from a recipe; isb removes only the images it built (use incus for others)"
        )))
    }
}

/// How the recipe runs: as its own program when it starts with `#!`, else
/// with /bin/sh.
pub fn recipe_argv(recipe: &str) -> Vec<&'static str> {
    if recipe.starts_with("#!") {
        vec![RECIPE_PATH]
    } else {
        vec!["/bin/sh", RECIPE_PATH]
    }
}

/// The environment the recipe runs with.
pub fn recipe_env() -> [(&'static str, &'static str); 4] {
    [
        (
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        ),
        ("DEBIAN_FRONTEND", "noninteractive"),
        ("HOME", "/root"),
        ("LANG", "C.UTF-8"),
    ]
}

/// The properties a published image carries.
pub fn properties(b: &ImageBuild) -> Value {
    json!({
        "description": b.description(),
        PROP_MARK: "1",
        PROP_RECIPE: b.recipe_sha256(),
        PROP_BASE: b.base,
        PROP_BUILT_BY: b.built_by,
        PROP_BUILT_AT: crate::stack::now_secs().to_string(),
    })
}

/// The build container's name: `wsimg-<name>-<random>`, within incus' 63.
fn container_name(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    format!(
        "wsimg-{}-{}{}",
        stem.trim_end_matches('-'),
        crate::stack::new_id(),
        crate::stack::new_id()
    )
}

/// What a finished build reports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Built {
    pub name: String,
    pub fingerprint: String,
    /// The image's size in bytes, when incus said.
    pub size: Option<u64>,
    /// Nothing was built: the image was up to date.
    pub up_to_date: bool,
    /// The image the alias named before, deleted when nothing else named it.
    pub replaced: Option<String>,
    pub seconds: u64,
}

/// Builds in progress, by name: one at a time per name.
static BUILDING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

struct Building(String);

impl Drop for Building {
    fn drop(&mut self) {
        BUILDING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

fn host(base: &Client) -> Client {
    base.clone().project("default")
}

/// The image an alias names on this host, if any.
pub fn existing(base: &Client, name: &str) -> Result<Option<Existing>> {
    let h = host(base);
    let Some(a) = h.get_opt(&format!("/1.0/images/aliases/{}", encode_segment(name)))? else {
        return Ok(None);
    };
    let fp = a["target"].as_str().unwrap_or_default();
    let img = h.get(&format!("/1.0/images/{}", encode_segment(fp)))?;
    Ok(Some(Existing::from_image(&img)))
}

/// The images isb built on this host, newest first: alias, description,
/// size, fingerprint and the build's properties.
pub fn list(base: &Client) -> Result<Vec<Value>> {
    let all = host(base).get("/1.0/images?recursion=1")?;
    let mut out: Vec<(u64, Value)> = all
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["properties"][PROP_MARK].as_str() == Some("1"))
        .map(|i| {
            let p = &i["properties"];
            let at: u64 = p[PROP_BUILT_AT]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let aliases: Vec<&str> = i["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["name"].as_str())
                .collect();
            let v = json!({
                "name": aliases.first(),
                "aliases": aliases,
                "fingerprint": i["fingerprint"],
                "size": i["size"],
                "description": p["description"],
                "base": p[PROP_BASE],
                "recipe_sha256": p[PROP_RECIPE],
                "default_recipe": p[PROP_RECIPE].as_str() == Some(recipe_sha256(DEFAULT_RECIPE).as_str()),
                "built_by": p[PROP_BUILT_BY],
                "built_at": at,
            });
            (at, v)
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

/// Remove an image isb built, by alias: the alias, and the image when no
/// other alias names it.
pub fn remove(base: &Client, name: &str) -> Result<Existing> {
    check_name(name)?;
    let e = existing(base, name)?.ok_or_else(|| Error::NotFound(format!("image {name}")))?;
    removable(name, &e)?;
    let h = host(base);
    let t = h.timeouts.other;
    h.mutate(
        "DELETE",
        &format!("/1.0/images/aliases/{}", encode_segment(name)),
        None,
        &format!("remove alias {name}"),
        t,
    )?;
    if e.aliases.iter().all(|a| a == name) {
        h.mutate(
            "DELETE",
            &format!("/1.0/images/{}", encode_segment(&e.fingerprint)),
            None,
            &format!("delete image {name}"),
            t,
        )?;
    }
    Ok(e)
}

/// The `isb-system` project, made with no registry in it when the host has
/// none yet: isb's own instances live there, never in an org.
fn system_project(base: &Client) -> Result<Client> {
    let h = host(base);
    let p = crate::registry::PROJECT;
    if h.get_opt(&format!("/1.0/projects/{p}"))?.is_none() {
        h.mutate(
            "POST",
            "/1.0/projects",
            Some(&json!({
                "name": p,
                "description": "isb system services (not an org)",
                "config": {
                    "features.images": "false",
                    "features.profiles": "true",
                    "features.storage.volumes": "true",
                    "features.networks": "false",
                },
            })),
            &format!("create project {p}"),
            h.timeouts.other,
        )?;
    }
    Ok(base.clone().project(p))
}

/// Build (or find up to date) the image `b` describes, logging each step
/// and every line the recipe prints.
pub fn build(base: &Client, b: &ImageBuild, log: &mut dyn FnMut(&str)) -> Result<Built> {
    check(b)?;
    let started = Instant::now();
    let deadline = started + b.timeout;
    if !BUILDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(b.name.clone())
    {
        return Err(Error::invalid(format!(
            "image {} is being built already",
            b.name
        )));
    }
    let _busy = Building(b.name.clone());
    let plan = plan(existing(base, &b.name)?.as_ref(), b)?;
    if let Plan::UpToDate { fingerprint } = plan {
        log(&format!(
            "image {} is up to date (same recipe and base); force rebuilds it",
            b.name
        ));
        return Ok(Built {
            name: b.name.clone(),
            fingerprint,
            size: None,
            up_to_date: true,
            replaced: None,
            seconds: 0,
        });
    }
    let s = system_project(base)?;
    let fingerprint = run_recipe(base, &s, b, deadline, log)?;
    let replaced = publish_alias(base, b, &fingerprint, &plan, log)?;
    let size = host(base)
        .get(&format!("/1.0/images/{}", encode_segment(&fingerprint)))
        .ok()
        .and_then(|i| i["size"].as_u64());
    let seconds = started.elapsed().as_secs();
    log(&format!(
        "published {} ({}) in {seconds}s",
        b.name,
        &fingerprint[..fingerprint.len().min(12)]
    ));
    Ok(Built {
        name: b.name.clone(),
        fingerprint,
        size,
        up_to_date: false,
        replaced,
        seconds,
    })
}

/// Launch the container, run the recipe, stop it and publish it without an
/// alias. Returns the new image's fingerprint; the container is deleted
/// whatever happens.
fn run_recipe(
    base: &Client,
    s: &Client,
    b: &ImageBuild,
    deadline: Instant,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let h = host(base);
    let name = container_name(&b.name);
    let _guard = Remove {
        client: s.clone(),
        name: name.clone(),
    };
    let pool = crate::sandbox::host_facts(&h)?.pick_pool(None)?;
    let net = uplink_network(&h)?;
    let src = crate::plan::ImageSource::parse(&b.base)?;
    log(&format!(
        "launching {name} from {} in project {} (network {net})",
        b.base,
        crate::registry::PROJECT
    ));
    s.mutate(
        "POST",
        "/1.0/instances",
        Some(&json!({
            "name": name,
            "type": "container",
            "source": src.to_api(None),
            "config": {"limits.cpu": "4", "limits.memory": "4GiB", "user.isb.workspace-image-build": b.name},
            "devices": {
                "root": {"type": "disk", "path": "/", "pool": pool},
                "eth0": {"type": "nic", "name": "eth0", "network": net},
            },
            "profiles": ["default"],
        })),
        &format!("create {name}"),
        remaining(deadline, "creating the build container")?.min(Duration::from_secs(1200)),
    )?;
    let state = format!("/1.0/instances/{}/state", encode_segment(&name));
    s.mutate(
        "PUT",
        &state,
        Some(&json!({"action": "start", "timeout": 60})),
        &format!("start {name}"),
        Duration::from_secs(300),
    )?;
    let sb = Sandbox::get(s, &name)?;
    ready::exec(&sb, deadline)?;
    ready::network(s, &name, &net, deadline, log)?;
    s.push_file(&name, RECIPE_PATH, b.recipe.as_bytes(), 0, 0, 0o700)?;
    log(&format!(
        "running the recipe ({} bytes, sha256 {})",
        b.recipe.len(),
        &b.recipe_sha256()[..12]
    ));
    let mut opts = ExecOptions::default().timeout(remaining(deadline, "running the recipe")?);
    for (k, v) in recipe_env() {
        opts = opts.env(k, v);
    }
    let code = stream_lines(&sb, &recipe_argv(&b.recipe), opts, log)?;
    if code != 0 {
        return Err(Error::invalid(format!(
            "the recipe failed (exit {code}); nothing was published"
        )));
    }
    let _ = sb.exec_with(
        ["rm", "-f", RECIPE_PATH],
        ExecOptions::default().timeout(Duration::from_secs(30)),
    );
    let stop = |force: bool| {
        s.mutate(
            "PUT",
            &state,
            Some(&json!({"action": "stop", "timeout": 120, "force": force})),
            &format!("stop {name}"),
            Duration::from_secs(180),
        )
    };
    if let Err(e) = stop(false) {
        log(&format!("a clean stop failed ({e}); forcing it"));
        stop(true)?;
    }
    log(&format!("publishing {}", b.name));
    let op = s.mutate(
        "POST",
        "/1.0/images",
        Some(&json!({
            "source": {"type": "instance", "name": name},
            "properties": properties(b),
        })),
        &format!("publish {}", b.name),
        remaining(deadline, "publishing the image")?.min(Duration::from_secs(1800)),
    )?;
    op["fingerprint"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::invalid("incus published the image without saying its fingerprint"))
}

/// Point the alias at the new image; delete the image it named before when
/// nothing else names that. A failure here deletes the new image.
fn publish_alias(
    base: &Client,
    b: &ImageBuild,
    fingerprint: &str,
    plan: &Plan,
    log: &mut dyn FnMut(&str),
) -> Result<Option<String>> {
    let h = host(base);
    let t = h.timeouts.other;
    let desc = b.description();
    let r = match plan {
        Plan::Replace { .. } => h.mutate(
            "PUT",
            &format!("/1.0/images/aliases/{}", encode_segment(&b.name)),
            Some(&json!({"target": fingerprint, "description": desc})),
            &format!("move alias {}", b.name),
            t,
        ),
        _ => h.mutate(
            "POST",
            "/1.0/images/aliases",
            Some(&json!({"name": b.name, "target": fingerprint, "description": desc})),
            &format!("add alias {}", b.name),
            t,
        ),
    };
    if let Err(e) = r {
        let _ = h.mutate(
            "DELETE",
            &format!("/1.0/images/{}", encode_segment(fingerprint)),
            None,
            "delete the unaliased image",
            t,
        );
        return Err(e);
    }
    let Plan::Replace { old } = plan else {
        return Ok(None);
    };
    if old == fingerprint {
        return Ok(None);
    }
    let still_named = h
        .get_opt(&format!("/1.0/images/{}", encode_segment(old)))?
        .is_some_and(|i| !Existing::from_image(&i).aliases.is_empty());
    if !still_named {
        match h.mutate(
            "DELETE",
            &format!("/1.0/images/{}", encode_segment(old)),
            None,
            "delete the replaced image",
            t,
        ) {
            Ok(_) => log(&format!(
                "deleted the image it replaced ({})",
                &old[..old.len().min(12)]
            )),
            Err(e) if e.is_not_found() => {}
            Err(e) => log(&format!("the image it replaced was not deleted: {e}")),
        }
    }
    Ok(Some(old.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(recipe: &str) -> ImageBuild {
        ImageBuild {
            name: "team-box".into(),
            recipe: recipe.into(),
            base: DEFAULT_BASE.into(),
            description: None,
            timeout: DEFAULT_TIMEOUT,
            force: false,
            built_by: "a@x.io".into(),
        }
    }

    fn image(ours: bool, recipe: &str, aliases: &[&str]) -> Existing {
        Existing {
            fingerprint: "f00d".into(),
            ours,
            recipe_sha256: Some(recipe_sha256(recipe)),
            base: Some(DEFAULT_BASE.into()),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn the_plan_creates_skips_replaces_and_never_takes_another_image() {
        let b = req("apt-get install -y git\n");
        assert_eq!(plan(None, &b).unwrap(), Plan::Create);
        // isb's image from the same recipe and base: nothing to do.
        let same = image(true, &b.recipe, &["team-box"]);
        assert_eq!(
            plan(Some(&same), &b).unwrap(),
            Plan::UpToDate {
                fingerprint: "f00d".into()
            }
        );
        // Forced, or another recipe, or another base: replace it.
        let forced = ImageBuild {
            force: true,
            ..b.clone()
        };
        assert_eq!(
            plan(Some(&same), &forced).unwrap(),
            Plan::Replace { old: "f00d".into() }
        );
        let other = image(true, "echo old\n", &["team-box"]);
        assert!(matches!(
            plan(Some(&other), &b).unwrap(),
            Plan::Replace { .. }
        ));
        let on_debian = ImageBuild {
            base: "images:debian/12".into(),
            ..b.clone()
        };
        assert!(matches!(
            plan(Some(&same), &on_debian).unwrap(),
            Plan::Replace { .. }
        ));
        // An image isb did not build (dev-base): refused, whatever is asked.
        let theirs = image(false, &b.recipe, &["team-box"]);
        assert!(plan(Some(&theirs), &forced).is_err());
    }

    #[test]
    fn only_isbs_images_can_be_removed() {
        let ours = Existing::from_image(&json!({
            "fingerprint": "abc",
            "aliases": [{"name": "isb-workspace"}],
            "properties": {PROP_MARK: "1", PROP_RECIPE: "x"}
        }));
        assert!(ours.ours);
        assert_eq!(ours.aliases, ["isb-workspace"]);
        assert!(removable("isb-workspace", &ours).is_ok());
        let dev_base = Existing::from_image(&json!({
            "fingerprint": "def",
            "aliases": [{"name": "dev-base"}],
            "properties": {"description": "dev-base: Ubuntu 24.04"}
        }));
        assert!(!dev_base.ours);
        let e = removable("dev-base", &dev_base).unwrap_err().to_string();
        assert!(e.contains("not built by isb"), "{e}");
        // A property that only looks like the mark is not it.
        let fake = Existing::from_image(&json!({"properties": {PROP_MARK: "true"}}));
        assert!(!fake.ours);
    }

    #[test]
    fn the_recipe_runs_as_root_with_a_plain_environment() {
        assert_eq!(recipe_argv("#!/bin/bash\necho hi\n"), [RECIPE_PATH]);
        assert_eq!(recipe_argv("echo hi\n"), ["/bin/sh", RECIPE_PATH]);
        let env: Vec<&str> = recipe_env().iter().map(|(k, _)| *k).collect();
        assert_eq!(env, ["PATH", "DEBIAN_FRONTEND", "HOME", "LANG"]);
        let b = req("echo hi\n");
        let p = properties(&b);
        assert_eq!(p[PROP_MARK], "1");
        assert_eq!(p[PROP_RECIPE], recipe_sha256("echo hi\n"));
        assert_eq!(p[PROP_BASE], DEFAULT_BASE);
        assert_eq!(p[PROP_BUILT_BY], "a@x.io");
        assert!(p["description"].as_str().unwrap().contains(DEFAULT_BASE));
        let n = container_name("team.box");
        assert!(n.starts_with("wsimg-team-box-") && n.len() <= 63, "{n}");
    }

    #[test]
    fn requests_are_checked_before_anything_is_made() {
        assert!(check(&req("echo hi")).is_ok());
        assert!(check(&ImageBuild::default_recipe("x")).is_ok());
        for bad in ["", "Box", "-x", "a/b", "images:ubuntu", &"a".repeat(64)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check(&req("  \n")).is_err());
        assert!(check(&req(&"x".repeat(MAX_RECIPE + 1))).is_err());
        let slow = ImageBuild {
            timeout: MAX_TIMEOUT + Duration::from_secs(1),
            ..req("echo")
        };
        assert!(check(&slow).is_err());
    }

    #[test]
    fn the_default_recipe_installs_outside_the_home_and_bakes_in_no_credentials() {
        let r = DEFAULT_RECIPE;
        assert!(r.starts_with("#!/bin/sh"));
        for tool in [
            "useradd -m -u 1000",
            "openssh-server",
            "build-essential",
            "MISE_INSTALL_PATH=/usr/local/bin/mise",
            "mise install --system",
            "/usr/local/bin/claude",
            "PI_INSTALL_DIR=/usr/local/bin",
            "HERDR_INSTALL_DIR=/usr/local/bin",
            "rm -f /etc/ssh/ssh_host_",
        ] {
            assert!(r.contains(tool), "{tool}");
        }
        for secret in ["API_KEY", "TOKEN=", "PASSWORD", "authorized_keys"] {
            assert!(!r.contains(secret), "{secret}");
        }
    }
}
