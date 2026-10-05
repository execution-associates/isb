//! Does a remote image exist? Asked before an app or a stack service names
//! one, so a typo is refused up front instead of deployed into a replica
//! that fails to pull, and asked again at each deploy, where its answer is
//! also the digest the image is pinned to.
//!
//! The registry is asked the way incus will ask it when it pulls: through
//! `skopeo inspect` (which incus itself runs for OCI images), anonymously
//! unless the daemon's user has registry credentials configured for
//! skopeo. `skopeo inspect` picks the host's platform from a multi-arch
//! index, so an image without one for this machine is "not found" too.
//!
//! Only a registry that answers "no such manifest" refuses. A registry that
//! cannot be reached, or that wants credentials (a private image, or on
//! Docker Hub a repository that does not exist), is reported as a warning:
//! being offline must not stop anyone from changing an app.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::plan::ImageSource;

/// How long a check at create or update waits for the registry.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the check at deploy waits (it also resolves the digest).
pub const DEPLOY_TIMEOUT: Duration = Duration::from_secs(60);

/// What a registry said about an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// It exists; its digest when the registry gave one.
    Found(Option<String>),
    /// The registry answered that there is no such image (or none for this
    /// platform): why, in the registry's words.
    NotFound(String),
    /// The registry wants credentials: a private image, or (Docker Hub,
    /// quay) a repository that does not exist.
    Denied(String),
    /// No answer: the registry is unreachable, skopeo is missing or timed
    /// out. Why.
    Unknown(String),
}

impl Probe {
    /// The digest, when the image was found with one.
    pub fn digest(&self) -> Option<&str> {
        match self {
            Probe::Found(d) => d.as_deref(),
            _ => None,
        }
    }
}

/// The image an isb reference names on its registry, as skopeo writes it
/// (`docker://docker.io/library/nginx:1.27`), and the registry's name for
/// people. `None` for what is not a remote OCI image: a local alias, an
/// incus simplestreams image, or `registry:` (the org's own, which the
/// stack controller resolves).
pub fn remote(image: &str) -> Option<(String, String)> {
    let src = ImageSource::parse(image).ok()?;
    if !src.is_oci() {
        return None;
    }
    let host = src.server.as_deref()?.strip_prefix("https://")?;
    let name = match host {
        "docker.io" => "Docker Hub".to_string(),
        "ghcr.io" => "GitHub Container Registry".to_string(),
        h => h.to_string(),
    };
    Some((format!("docker://{host}/{}", src.alias), name))
}

/// Ask the image's registry, giving up after `timeout`. An image that is
/// not a remote OCI image is `Found(None)`: there is nothing to ask.
pub fn probe(image: &str, timeout: Duration) -> Probe {
    let Some((r, _)) = remote(image) else {
        return Probe::Found(None);
    };
    let child = Command::new("skopeo")
        .args(["inspect", "--no-tags", "--format", "{{.Digest}}", &r])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Probe::Unknown(format!("cannot run skopeo: {e}")),
    };
    // Read both pipes on threads: a full pipe would stall skopeo.
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let o = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let e = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.success(),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Probe::Unknown(format!("no answer within {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Probe::Unknown(format!("skopeo: {e}")),
        }
    };
    let stdout = o.join().unwrap_or_default();
    let stderr = e.join().unwrap_or_default();
    if ok {
        let d = stdout.trim();
        let digest = (d.starts_with("sha256:") && d.len() == 71).then(|| d.to_string());
        return Probe::Found(digest);
    }
    classify(&stderr)
}

/// What a failed `skopeo inspect` said, sorted by what it means.
pub fn classify(stderr: &str) -> Probe {
    let why = reason(stderr);
    let l = stderr.to_ascii_lowercase();
    if [
        "manifest unknown",
        "name unknown",
        "no image found in manifest list",
        "not found: manifest",
    ]
    .iter()
    .any(|k| l.contains(k))
    {
        Probe::NotFound(why)
    } else if [
        "denied",
        "unauthorized",
        "authentication required",
        "401",
        "403 forbidden",
    ]
    .iter()
    .any(|k| l.contains(k))
    {
        Probe::Denied(why)
    } else {
        Probe::Unknown(why)
    }
}

/// The useful end of skopeo's message: after its last `": "` wrapper, so
/// `reading manifest whoami in docker.io/library/traefik: manifest unknown`
/// becomes `manifest unknown`.
fn reason(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let msg = match line.split_once("msg=\"") {
        Some((_, m)) => quoted(m),
        None => line.trim().to_string(),
    };
    let tail = msg.rsplit(": ").next().unwrap_or(&msg).trim();
    if tail.is_empty() {
        "no reason given".to_string()
    } else {
        tail.to_string()
    }
}

/// The text of a quoted logfmt value, up to its closing quote (incus wraps
/// skopeo's line in more text), with `\"` unescaped.
fn quoted(m: &str) -> String {
    let mut out = String::new();
    let mut chars = m.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '"' => break,
            c => out.push(c),
        }
    }
    out
}

/// The image someone more likely meant, for the commonest slip: a Docker
/// Hub `owner:name` written for `owner/name` (`docker:traefik:whoami`, for
/// `docker:traefik/whoami`). Only a reference with no `/` and one tag qualifies.
pub fn suggestion(image: &str) -> Option<String> {
    let rest = image.strip_prefix("docker:")?;
    if rest.contains('/') || rest.contains('@') {
        return None;
    }
    let (owner, name) = rest.split_once(':')?;
    if owner.is_empty() || name.is_empty() || name.contains(':') {
        return None;
    }
    Some(format!("docker:{owner}/{name}"))
}

/// Check an image before it is saved. `Err` when its registry says it does
/// not exist; `Ok(Some(warning))` when that could not be confirmed;
/// `Ok(None)` when it exists or is not a remote image.
pub fn check(image: &str, probe: &dyn Fn(&str) -> Probe) -> Result<Option<String>> {
    let Some((_, registry)) = remote(image) else {
        return Ok(None);
    };
    let found = |p: &Probe| matches!(p, Probe::Found(_));
    let better = || suggestion(image).filter(|s| found(&probe(s)));
    match probe(image) {
        Probe::Found(_) => Ok(None),
        Probe::NotFound(why) => Err(Error::invalid(not_found(image, &registry, &why, better()))),
        Probe::Denied(why) => Ok(Some(match better() {
            Some(s) => format!(
                "{registry} refused to show image {image} without credentials ({why}): it is private or does not exist. Did you mean {s}?"
            ),
            None => format!(
                "{registry} refused to show image {image} without credentials ({why}): if it is private, the host needs credentials to pull it; if it does not exist, the deploy will fail"
            ),
        })),
        Probe::Unknown(why) => Ok(Some(format!(
            "could not check image {image} on {registry} ({why}); saved unchecked"
        ))),
    }
}

/// The refusal for an image its registry does not have.
pub fn not_found(image: &str, registry: &str, why: &str, better: Option<String>) -> String {
    let mut m = format!("image {image} not found on {registry} ({why})");
    match better {
        Some(s) => m.push_str(&format!(": did you mean {s}?")),
        None => m.push_str(
            ": check the name and tag (Docker Hub images are docker:NAME[:TAG] or docker:OWNER/NAME[:TAG], e.g. docker:nginx:1.27 or docker:traefik/whoami)",
        ),
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNKNOWN: &str = r#"time="2026-10-05T04:38:08Z" level=fatal msg="Error parsing image name \"docker://docker.io/library/traefik:whoami\": reading manifest whoami in docker.io/library/traefik: manifest unknown""#;
    const DENIED: &str = r#"time="x" level=fatal msg="Error parsing image name \"docker://docker.io/library/ngnixx:latest\": reading manifest latest in docker.io/library/ngnixx: requested access to the resource is denied""#;
    const OFFLINE: &str = r#"time="x" level=fatal msg="Error parsing image name \"docker://nosuch.invalid/a/b:1\": pinging container registry nosuch.invalid: Get \"https://nosuch.invalid/v2/\": dial tcp: lookup nosuch.invalid on 127.0.0.53:53: no such host""#;
    const GHCR: &str = r#"time="x" level=fatal msg="Error parsing image name \"docker://ghcr.io/o/a:v1\": Requesting bearer token: received unexpected HTTP status: 403 Forbidden""#;
    const PLATFORM: &str = r#"time="x" level=fatal msg="Error parsing image name \"docker://docker.io/o/a:1\": choosing image instance: no image found in manifest list for architecture \"amd64\", variant \"\", OS \"linux\"""#;

    #[test]
    fn skopeo_errors_are_sorted_by_what_they_mean() {
        assert_eq!(
            classify(UNKNOWN),
            Probe::NotFound("manifest unknown".into())
        );
        assert!(matches!(classify(PLATFORM), Probe::NotFound(w) if w.ends_with("OS \"linux\"")));
        assert_eq!(
            classify(DENIED),
            Probe::Denied("requested access to the resource is denied".into())
        );
        assert!(matches!(classify(GHCR), Probe::Denied(_)));
        assert!(matches!(classify(OFFLINE), Probe::Unknown(w) if w == "no such host"));
        assert!(matches!(classify(""), Probe::Unknown(_)));
    }

    #[test]
    fn only_remote_oci_images_are_asked_about() {
        assert_eq!(
            remote("docker:traefik:whoami").unwrap(),
            (
                "docker://docker.io/library/traefik:whoami".to_string(),
                "Docker Hub".to_string()
            )
        );
        assert_eq!(
            remote("docker:traefik/whoami").unwrap().0,
            "docker://docker.io/traefik/whoami:latest"
        );
        assert_eq!(
            remote("ghcr:org/app:v1").unwrap(),
            (
                "docker://ghcr.io/org/app:v1".to_string(),
                "GitHub Container Registry".to_string()
            )
        );
        assert_eq!(
            remote("oci:reg.example.com:5000/a/b:1").unwrap().1,
            "reg.example.com:5000"
        );
        for local in [
            "dev-base",
            "images:debian/12",
            "registry:web:v1",
            "nonsense:x",
        ] {
            assert!(remote(local).is_none(), "{local}");
        }
    }

    #[test]
    fn a_colon_for_a_slash_is_suggested_back() {
        assert_eq!(
            suggestion("docker:traefik:whoami").as_deref(),
            Some("docker:traefik/whoami")
        );
        for none in [
            "docker:traefik/whoami",
            "docker:nginx",
            "docker:a:b:c",
            "docker:a@sha256:x",
            "ghcr:a:b",
        ] {
            assert!(suggestion(none).is_none(), "{none}");
        }
    }

    fn registry(found: &'static [&'static str], answer: Probe) -> impl Fn(&str) -> Probe {
        move |i: &str| {
            if found.contains(&i) {
                Probe::Found(Some(format!("sha256:{}", "a".repeat(64))))
            } else {
                answer.clone()
            }
        }
    }

    #[test]
    fn a_missing_image_is_refused_with_what_was_meant() {
        let p = registry(
            &["docker:traefik/whoami"],
            Probe::NotFound("manifest unknown".into()),
        );
        let e = check("docker:traefik:whoami", &p).unwrap_err().to_string();
        assert!(
            e.contains("image docker:traefik:whoami not found on Docker Hub (manifest unknown): did you mean docker:traefik/whoami?"),
            "{e}"
        );
        let e = check("docker:nginx:nosuchtag", &p).unwrap_err().to_string();
        assert!(
            e.contains("not found on Docker Hub") && e.contains("docker:nginx:1.27"),
            "{e}"
        );
        assert_eq!(check("docker:traefik/whoami", &p).unwrap(), None);
    }

    #[test]
    fn what_cannot_be_checked_warns_and_never_blocks() {
        let p = registry(&[], Probe::Unknown("no such host".into()));
        let w = check("oci:nosuch.invalid/a/b:1", &p).unwrap().unwrap();
        assert!(
            w.contains("could not check") && w.contains("no such host"),
            "{w}"
        );
        let p = registry(&[], Probe::Denied("denied".into()));
        let w = check("ghcr:o/private:v1", &p).unwrap().unwrap();
        assert!(w.contains("without credentials"), "{w}");
        let p = registry(&["docker:traefik/whoami"], Probe::Denied("denied".into()));
        let w = check("docker:traefik:whoami", &p).unwrap().unwrap();
        assert!(w.contains("Did you mean docker:traefik/whoami?"), "{w}");
        // Local and incus images are never asked about.
        let never = |_: &str| -> Probe { panic!("asked") };
        assert_eq!(check("dev-base", &never).unwrap(), None);
        assert_eq!(check("registry:web:v1", &never).unwrap(), None);
    }
}
