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
    /// A pull (merge) request opened, updated or closed: previews.
    PullRequest(PullRequest),
    /// Anything else (issues, comments, ...): ignored.
    Other(String),
}

/// What happened to a pull request, as far as previews care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrAction {
    /// Opened or reopened: build and deploy a preview.
    Open,
    /// New commits on its head: rebuild the preview.
    Sync,
    /// Closed, merged or not: remove the preview.
    Close { merged: bool },
    /// Edited, labelled, reviewed, ...: nothing to do.
    Other,
}

/// A pull request event (GitHub and Gitea/Forgejo `pull_request`, GitLab
/// `Merge Request Hook`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub action: PrAction,
    /// The action as the sender named it (`synchronize`, `update`, ...).
    pub raw_action: String,
    /// GitHub/Gitea `number`, GitLab `iid`.
    pub number: u64,
    pub head_sha: Option<String>,
    /// The branch the changes are on (in the head repository).
    pub head_ref: String,
    /// The branch it would merge into.
    pub base_ref: String,
    /// The head is in another repository than the base: a fork, whose code
    /// nobody with push access wrote.
    pub fork: bool,
    pub title: String,
}

impl PullRequest {
    /// The ref the forge keeps the request's head under, in the base
    /// repository (so a fork's commits are fetched from the base).
    pub fn head_ref_in_base(provider: Provider, number: u64) -> String {
        match provider {
            Provider::GitLab => format!("refs/merge-requests/{number}/head"),
            _ => format!("refs/pull/{number}/head"),
        }
    }
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
    // Gitea and Forgejo also send GitHub's header: theirs decides, so the
    // sender is known for what follows (pull request refs, status API).
    if let Some(sig) = header("x-gitea-signature").or_else(|| header("x-forgejo-signature")) {
        return hmac_ok(secret, body, sig.trim())
            .then_some(Provider::Gitea)
            .ok_or(Refusal::Invalid);
    }
    if let Some(sig) = header("x-hub-signature-256") {
        let hex = sig.trim().strip_prefix("sha256=").ok_or(Refusal::Invalid)?;
        return hmac_ok(secret, body, hex)
            .then_some(Provider::GitHub)
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
        Some("pull_request" | "merge request hook") => {
            return match json.as_ref().and_then(|j| pull_request(provider, j)) {
                Some(pr) => Event::PullRequest(pr),
                None => Event::Other("pull request without a number".into()),
            };
        }
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

/// A pull request from its payload: GitHub and Gitea/Forgejo share a shape
/// (`action`, `number`, `pull_request.{head,base}`), GitLab has its own
/// (`object_attributes`).
fn pull_request(provider: Provider, j: &Value) -> Option<PullRequest> {
    let s = |p: &str| j.pointer(p).and_then(Value::as_str).map(String::from);
    if provider == Provider::GitLab
        || j.get("object_kind").and_then(Value::as_str) == Some("merge_request")
    {
        let a = j.get("object_attributes")?;
        let number = a.get("iid").and_then(Value::as_u64)?;
        let raw = s("/object_attributes/action").unwrap_or_default();
        // An `update` with `oldrev` moved the head; without it only the
        // title, labels or the like changed.
        let action = match raw.as_str() {
            "open" | "reopen" => PrAction::Open,
            "update" if a.get("oldrev").is_some_and(|v| v.is_string()) => PrAction::Sync,
            "close" => PrAction::Close { merged: false },
            "merge" => PrAction::Close { merged: true },
            _ => PrAction::Other,
        };
        let src = a.get("source_project_id").and_then(Value::as_u64);
        let dst = a.get("target_project_id").and_then(Value::as_u64);
        return Some(PullRequest {
            action,
            raw_action: raw,
            number,
            head_sha: s("/object_attributes/last_commit/id"),
            head_ref: s("/object_attributes/source_branch").unwrap_or_default(),
            base_ref: s("/object_attributes/target_branch").unwrap_or_default(),
            // Unknown counts as a fork: the safe side.
            fork: src.is_none() || src != dst,
            title: s("/object_attributes/title").unwrap_or_default(),
        });
    }
    let pr = j.get("pull_request")?;
    let number = j
        .get("number")
        .and_then(Value::as_u64)
        .or_else(|| pr.get("number").and_then(Value::as_u64))?;
    let raw = s("/action").unwrap_or_default();
    let merged = pr.get("merged").and_then(Value::as_bool) == Some(true);
    let action = match raw.as_str() {
        "opened" | "reopened" => PrAction::Open,
        // GitHub `synchronize`, Gitea/Forgejo `synchronized`.
        "synchronize" | "synchronized" => PrAction::Sync,
        "closed" => PrAction::Close { merged },
        _ => PrAction::Other,
    };
    // A head repository that is gone (a deleted fork) or is not the base
    // repository is a fork.
    let head_repo = s("/pull_request/head/repo/full_name");
    let base_repo = s("/pull_request/base/repo/full_name").or_else(|| s("/repository/full_name"));
    let fork = match (&head_repo, &base_repo) {
        (Some(h), Some(b)) => !h.eq_ignore_ascii_case(b),
        _ => true,
    };
    Some(PullRequest {
        action,
        raw_action: raw,
        number,
        head_sha: s("/pull_request/head/sha"),
        head_ref: s("/pull_request/head/ref").unwrap_or_default(),
        base_ref: s("/pull_request/base/ref").unwrap_or_default(),
        fork,
        title: s("/pull_request/title").unwrap_or_default(),
    })
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
        // Gitea sends GitHub's header too; it is still Gitea.
        let both = headers(&[
            ("X-Gitea-Signature", &sig),
            ("X-Hub-Signature-256", &format!("sha256={sig}")),
        ]);
        assert_eq!(verify(SECRET, &both, None, body), Ok(Provider::Gitea));
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
        // A pull request event is not a push.
        let pr = br#"{"action":"opened","number":3,"pull_request":{"head":{"sha":"abc","ref":"f","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}}}}"#;
        assert!(matches!(
            event(
                Provider::GitHub,
                &headers(&[("X-GitHub-Event", "pull_request")]),
                pr
            ),
            Event::PullRequest(_)
        ));
        assert!(matches!(
            event(Provider::Generic, &headers(&[]), push),
            Event::Push { .. }
        ));
    }

    fn pr_of(provider: Provider, h: &[(&str, &str)], body: &str) -> PullRequest {
        match event(provider, &headers(h), body.as_bytes()) {
            Event::PullRequest(p) => p,
            e => panic!("not a pull request: {e:?}"),
        }
    }

    /// GitHub's `pull_request` payload, trimmed to what is read.
    fn github(action: &str, head_repo: &str, merged: bool) -> String {
        format!(
            r#"{{"action":"{action}","number":42,"pull_request":{{"number":42,"title":"Add a thing","merged":{merged},
            "head":{{"ref":"feature","sha":"0123456789abcdef0123456789abcdef01234567","repo":{{"full_name":"{head_repo}","fork":{}}}}},
            "base":{{"ref":"main","sha":"fedcba","repo":{{"full_name":"acme/web"}}}}}},
            "repository":{{"full_name":"acme/web"}}}}"#,
            head_repo != "acme/web"
        )
    }

    #[test]
    fn github_pull_requests() {
        let h = [("X-GitHub-Event", "pull_request")];
        let p = pr_of(Provider::GitHub, &h, &github("opened", "acme/web", false));
        assert_eq!(p.action, PrAction::Open);
        assert_eq!(p.number, 42);
        assert_eq!(p.head_ref, "feature");
        assert_eq!(p.base_ref, "main");
        assert_eq!(p.title, "Add a thing");
        assert_eq!(
            p.head_sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(!p.fork);
        let p = pr_of(Provider::GitHub, &h, &github("reopened", "acme/web", false));
        assert_eq!(p.action, PrAction::Open);
        let p = pr_of(
            Provider::GitHub,
            &h,
            &github("synchronize", "acme/web", false),
        );
        assert_eq!(p.action, PrAction::Sync);
        let p = pr_of(Provider::GitHub, &h, &github("closed", "acme/web", false));
        assert_eq!(p.action, PrAction::Close { merged: false });
        let p = pr_of(Provider::GitHub, &h, &github("closed", "acme/web", true));
        assert_eq!(p.action, PrAction::Close { merged: true });
        let p = pr_of(Provider::GitHub, &h, &github("labeled", "acme/web", false));
        assert_eq!(p.action, PrAction::Other);
        // From a fork; from a fork since deleted (head.repo null).
        let p = pr_of(
            Provider::GitHub,
            &h,
            &github("opened", "mallory/web", false),
        );
        assert!(p.fork);
        let gone = r#"{"action":"synchronize","number":5,"pull_request":{"head":{"ref":"x","sha":"ab","repo":null},"base":{"ref":"main","repo":{"full_name":"acme/web"}}}}"#;
        assert!(pr_of(Provider::GitHub, &h, gone).fork);
        // Case differences in the name are the same repository.
        let p = pr_of(Provider::GitHub, &h, &github("opened", "ACME/Web", false));
        assert!(!p.fork);
        // A pull request without a number is nothing to act on.
        assert!(matches!(
            event(Provider::GitHub, &headers(&h), br#"{"action":"opened"}"#),
            Event::Other(_)
        ));
    }

    #[test]
    fn gitea_pull_requests() {
        // Gitea/Forgejo send `pull_request` for open, sync and close, with
        // `synchronized` (not GitHub's `synchronize`).
        let body = |action: &str, head: &str, merged: bool| {
            format!(
                r#"{{"action":"{action}","number":7,"pull_request":{{"id":99,"number":7,"title":"t","merged":{merged},
                "head":{{"label":"f","ref":"f","sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","repo_id":2,"repo":{{"id":2,"full_name":"{head}"}}}},
                "base":{{"label":"main","ref":"main","sha":"b","repo_id":1,"repo":{{"id":1,"full_name":"isb/app"}}}}}},
                "repository":{{"id":1,"full_name":"isb/app"}}}}"#
            )
        };
        let h = [
            ("X-Gitea-Event", "pull_request"),
            ("X-Gitea-Event-Type", "pull_request"),
        ];
        let p = pr_of(Provider::Gitea, &h, &body("opened", "isb/app", false));
        assert_eq!((p.action, p.number, p.fork), (PrAction::Open, 7, false));
        let hs = [
            ("X-Gitea-Event", "pull_request"),
            ("X-Gitea-Event-Type", "pull_request_sync"),
        ];
        let p = pr_of(
            Provider::Gitea,
            &hs,
            &body("synchronized", "isb/app", false),
        );
        assert_eq!(p.action, PrAction::Sync);
        let p = pr_of(Provider::Gitea, &h, &body("closed", "isb/app", true));
        assert_eq!(p.action, PrAction::Close { merged: true });
        let p = pr_of(Provider::Gitea, &h, &body("opened", "someone/app", false));
        assert!(p.fork);
        let f = [("X-Forgejo-Event", "pull_request")];
        assert_eq!(
            pr_of(Provider::Gitea, &f, &body("reopened", "isb/app", false)).action,
            PrAction::Open
        );
        // Reviews and comments on a pull request are other events.
        let rv = [("X-Gitea-Event", "pull_request_approved")];
        assert!(matches!(
            event(
                Provider::Gitea,
                &headers(&rv),
                body("reviewed", "isb/app", false).as_bytes()
            ),
            Event::Other(_)
        ));
    }

    #[test]
    fn gitlab_merge_requests() {
        let body = |action: &str, oldrev: bool, src: u64| {
            format!(
                r#"{{"object_kind":"merge_request","project":{{"id":1,"path_with_namespace":"acme/web"}},
                "object_attributes":{{"iid":12,"title":"MR","action":"{action}","state":"opened",
                "source_branch":"feat","target_branch":"main","source_project_id":{src},"target_project_id":1,
                {}"last_commit":{{"id":"cccccccccccccccccccccccccccccccccccccccc","message":"m"}}}}}}"#,
                if oldrev {
                    r#""oldrev":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#
                } else {
                    ""
                }
            )
        };
        let h = [("X-Gitlab-Event", "Merge Request Hook")];
        let p = pr_of(Provider::GitLab, &h, &body("open", false, 1));
        assert_eq!((p.action, p.number, p.fork), (PrAction::Open, 12, false));
        assert_eq!(p.head_ref, "feat");
        assert_eq!(p.base_ref, "main");
        assert_eq!(
            p.head_sha.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccc")
        );
        assert_eq!(
            pr_of(Provider::GitLab, &h, &body("reopen", false, 1)).action,
            PrAction::Open
        );
        // An update with new commits carries oldrev; one without is a
        // title or label change.
        assert_eq!(
            pr_of(Provider::GitLab, &h, &body("update", true, 1)).action,
            PrAction::Sync
        );
        assert_eq!(
            pr_of(Provider::GitLab, &h, &body("update", false, 1)).action,
            PrAction::Other
        );
        assert_eq!(
            pr_of(Provider::GitLab, &h, &body("close", false, 1)).action,
            PrAction::Close { merged: false }
        );
        assert_eq!(
            pr_of(Provider::GitLab, &h, &body("merge", false, 1)).action,
            PrAction::Close { merged: true }
        );
        assert!(pr_of(Provider::GitLab, &h, &body("open", false, 2)).fork);
    }
}
