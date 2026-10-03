//! Push webhooks: who sent one, whether its signature holds, and what it
//! asks for.
//!
//! `POST /api/v1/webhooks/<org>/<app>` is reachable without a session; the
//! app's webhook secret (an org secret) is its only credential:
//! - GitHub: `X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the body>`;
//! - Gitea / Forgejo: `X-Gitea-Signature` / `X-Forgejo-Signature`: the hex
//!   HMAC-SHA256 of the body;
//! - GitLab: `X-Gitlab-Token`: the secret itself;
//! - anything else: `?token=<secret>`.
//!
//! Every comparison is constant-time.

use serde_json::Value;

/// A request's headers by lowercase name.
pub type Headers<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Which kind of sender, by the header that authenticated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    Gitea,
    GitLab,
    Generic,
}

/// What a verified request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A configuration check (GitHub's `ping`): answer 200, do nothing.
    Ping,
    /// A push to `reference` (`refs/heads/main`), now at `after`.
    Push {
        reference: String,
        after: Option<String>,
        message: Option<String>,
        deleted: bool,
    },
    /// A generic `?token=` call with no push payload: deploy.
    Trigger,
    /// Anything else (issues, pull requests, ...): ignored.
    Other(String),
}

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No credential this endpoint knows.
    Missing,
    /// A credential that does not match.
    Invalid,
}

/// Verify a request against the app's secret: the provider it came from.
pub fn verify(
    secret: &[u8],
    header: &Headers,
    query_token: Option<&str>,
    body: &[u8],
) -> Result<Provider, Refusal> {
    if let Some(sig) = header("x-hub-signature-256") {
        let hex = sig.trim().strip_prefix("sha256=").ok_or(Refusal::Invalid)?;
        return hmac_ok(secret, body, hex)
            .then_some(Provider::GitHub)
            .ok_or(Refusal::Invalid);
    }
    if let Some(sig) = header("x-gitea-signature").or_else(|| header("x-forgejo-signature")) {
        return hmac_ok(secret, body, sig.trim())
            .then_some(Provider::Gitea)
            .ok_or(Refusal::Invalid);
    }
    if let Some(tok) = header("x-gitlab-token") {
        return ct_eq(tok.trim().as_bytes(), secret)
            .then_some(Provider::GitLab)
            .ok_or(Refusal::Invalid);
    }
    if let Some(tok) = query_token {
        return ct_eq(tok.as_bytes(), secret)
            .then_some(Provider::Generic)
            .ok_or(Refusal::Invalid);
    }
    Err(Refusal::Missing)
}

fn hmac_ok(secret: &[u8], body: &[u8], hex: &str) -> bool {
    let Some(sig) = from_hex(hex) else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    ring::hmac::verify(&key, body, &sig).is_ok()
}

/// The hex HMAC-SHA256 a sender computes, for tests and for `isb app
/// webhook --test`.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    to_hex(ring::hmac::sign(&key, body).as_ref())
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    // Compare digests, so the time spent depends on neither length.
    let da = ring::digest::digest(&ring::digest::SHA256, a);
    let db = ring::digest::digest(&ring::digest::SHA256, b);
    let mut d = 0u8;
    for (x, y) in da.as_ref().iter().zip(db.as_ref()) {
        d |= x ^ y;
    }
    d == 0
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a verified request asks for, from its event header and body.
pub fn event(provider: Provider, header: &Headers, body: &[u8]) -> Event {
    let kind = match provider {
        Provider::GitHub => header("x-github-event"),
        Provider::Gitea => header("x-gitea-event").or_else(|| header("x-forgejo-event")),
        Provider::GitLab => header("x-gitlab-event"),
        Provider::Generic => header("x-github-event")
            .or_else(|| header("x-gitea-event"))
            .or_else(|| header("x-gitlab-event")),
    };
    let json: Option<Value> = serde_json::from_slice(body).ok();
    match kind.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("ping") => return Event::Ping,
        Some("push" | "push hook" | "tag push hook") => {}
        Some(other) => return Event::Other(other.to_string()),
        None if provider == Provider::Generic => {}
        None => return Event::Other("unknown".into()),
    }
    let Some(j) = json.filter(|j| j.get("ref").is_some()) else {
        return if provider == Provider::Generic {
            Event::Trigger
        } else {
            Event::Other("push without a ref".into())
        };
    };
    let s = |v: &Value| v.as_str().map(String::from);
    let after = j
        .get("after")
        .and_then(s)
        .or_else(|| j.get("checkout_sha").and_then(s));
    let zero = after
        .as_deref()
        .is_some_and(|a| a.bytes().all(|b| b == b'0'));
    let message = j
        .pointer("/head_commit/message")
        .and_then(s)
        .or_else(|| {
            j.get("commits")
                .and_then(Value::as_array)
                .and_then(|c| c.last())
                .and_then(|c| c.get("message"))
                .and_then(s)
        })
        .map(|m| m.lines().next().unwrap_or("").to_string());
    Event::Push {
        reference: j.get("ref").and_then(s).unwrap_or_default(),
        after: after.filter(|_| !zero),
        message,
        deleted: zero || j.get("deleted").and_then(Value::as_bool) == Some(true),
    }
}

/// A delivery id, for refusing a replayed delivery.
pub fn delivery_id(header: &Headers) -> Option<String> {
    header("x-github-delivery")
        .or_else(|| header("x-gitea-delivery"))
        .or_else(|| header("x-forgejo-delivery"))
        .or_else(|| header("x-gitlab-event-uuid"))
        .filter(|d| !d.is_empty() && d.len() <= 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(h: &[(&str, &str)]) -> Box<Headers<'static>> {
        let h: Vec<(String, String)> = h
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        Box::new(move |k: &str| {
            h.iter()
                .find(|(n, _)| n == &k.to_ascii_lowercase())
                .map(|(_, v)| v.clone())
        })
    }

    const SECRET: &[u8] = b"It's a Secret to Everybody";

    #[test]
    fn github_signature() {
        // GitHub's documented example.
        let body = b"Hello, World!";
        let want = "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert_eq!(sign(SECRET, body), want);
        let ok = headers(&[("X-Hub-Signature-256", &format!("sha256={want}"))]);
        assert_eq!(verify(SECRET, &ok, None, body), Ok(Provider::GitHub));
        // Another body, another secret, a mangled header: refused.
        assert_eq!(
            verify(SECRET, &ok, None, b"Hello, World?"),
            Err(Refusal::Invalid)
        );
        assert_eq!(verify(b"other", &ok, None, body), Err(Refusal::Invalid));
        let bare = headers(&[("X-Hub-Signature-256", want)]);
        assert_eq!(verify(SECRET, &bare, None, body), Err(Refusal::Invalid));
        let odd = headers(&[("X-Hub-Signature-256", "sha256=zz")]);
        assert_eq!(verify(SECRET, &odd, None, body), Err(Refusal::Invalid));
        // A signature header is decisive: a right ?token= does not rescue a
        // wrong signature.
        let wrong = headers(&[("X-Hub-Signature-256", "sha256=00")]);
        assert_eq!(
            verify(SECRET, &wrong, Some("It's a Secret to Everybody"), body),
            Err(Refusal::Invalid)
        );
    }

    #[test]
    fn gitea_gitlab_and_token() {
        let body = br#"{"ref":"refs/heads/main"}"#;
        let sig = sign(SECRET, body);
        let gitea = headers(&[("X-Gitea-Signature", &sig)]);
        assert_eq!(verify(SECRET, &gitea, None, body), Ok(Provider::Gitea));
        let forgejo = headers(&[("X-Forgejo-Signature", &sig)]);
        assert_eq!(verify(SECRET, &forgejo, None, body), Ok(Provider::Gitea));
        let gl = headers(&[("X-Gitlab-Token", "It's a Secret to Everybody")]);
        assert_eq!(verify(SECRET, &gl, None, body), Ok(Provider::GitLab));
        let gl_bad = headers(&[("X-Gitlab-Token", "It's a Secret to Everybod")]);
        assert_eq!(verify(SECRET, &gl_bad, None, body), Err(Refusal::Invalid));
        let none = headers(&[]);
        assert_eq!(
            verify(SECRET, &none, Some("It's a Secret to Everybody"), body),
            Ok(Provider::Generic)
        );
        assert_eq!(
            verify(SECRET, &none, Some("nope"), body),
            Err(Refusal::Invalid)
        );
        assert_eq!(verify(SECRET, &none, None, body), Err(Refusal::Missing));
    }

    #[test]
    fn events() {
        let push = br#"{"ref":"refs/heads/main","after":"abc123","head_commit":{"message":"fix: it\n\nbody"}}"#;
        let gh = headers(&[("X-GitHub-Event", "push")]);
        assert_eq!(
            event(Provider::GitHub, &gh, push),
            Event::Push {
                reference: "refs/heads/main".into(),
                after: Some("abc123".into()),
                message: Some("fix: it".into()),
                deleted: false,
            }
        );
        let ping = headers(&[("X-GitHub-Event", "ping")]);
        assert_eq!(event(Provider::GitHub, &ping, b"{}"), Event::Ping);
        // A signed non-push event (an issue, a replayed pull request) is not
        // a deploy.
        let issues = headers(&[("X-GitHub-Event", "issues")]);
        assert_eq!(
            event(Provider::GitHub, &issues, push),
            Event::Other("issues".into())
        );
        let gl = headers(&[("X-Gitlab-Event", "Push Hook")]);
        let gl_body = br#"{"ref":"refs/heads/dev","checkout_sha":"def","commits":[{"message":"one"},{"message":"two"}]}"#;
        assert_eq!(
            event(Provider::GitLab, &gl, gl_body),
            Event::Push {
                reference: "refs/heads/dev".into(),
                after: Some("def".into()),
                message: Some("two".into()),
                deleted: false,
            }
        );
        let del = br#"{"ref":"refs/heads/main","after":"0000000000000000000000000000000000000000","deleted":true}"#;
        assert!(matches!(
            event(Provider::Gitea, &headers(&[("X-Gitea-Event", "push")]), del),
            Event::Push {
                deleted: true,
                after: None,
                ..
            }
        ));
        assert_eq!(event(Provider::Generic, &headers(&[]), b""), Event::Trigger);
        assert!(matches!(
            event(Provider::Generic, &headers(&[]), push),
            Event::Push { .. }
        ));
    }
}
