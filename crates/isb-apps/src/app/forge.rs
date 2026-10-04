//! Commit statuses on the forge: a preview's state and URL, shown on the
//! pull request.
//!
//! GitHub (`POST /repos/{owner}/{repo}/statuses/{sha}`, `Authorization:
//! Bearer`) and Gitea/Forgejo (the same path under `/api/v1`, `Authorization:
//! token`) take the same body: `{state, target_url, description, context}`.
//! A status with the same `context` replaces the previous one, so each
//! preview deploy updates one line on the pull request.
//!
//! The token (an org secret) goes only to the API of the host the app's git
//! URL names, over HTTPS, unless `api_url` says otherwise.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};

/// The status line's name on the pull request.
pub const CONTEXT: &str = "isb/preview";

/// Which API shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    #[serde(alias = "GitHub")]
    Github,
    /// Gitea and Forgejo.
    #[serde(alias = "forgejo")]
    Gitea,
}

/// Where a preview's status goes and with which token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSettings {
    /// An org secret holding an API token that may write commit statuses
    /// (GitHub: `repo:status` / fine-grained "Commit statuses: write";
    /// Gitea: `write:repository`).
    pub token_secret: String,
    /// `github` or `gitea` (also Forgejo). Default: the webhook's sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ForgeKind>,
    /// The API base: `https://api.github.com`, `https://git.example.com/api/v1`.
    /// Default: derived from the git URL (github.com's API, or
    /// `https://<host>/api/v1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

impl StatusSettings {
    pub fn validate(&self) -> Result<()> {
        crate::secrets::validate_name(&self.token_secret)?;
        if let Some(u) = &self.api_url {
            if !(u.starts_with("https://") || u.starts_with("http://"))
                || u.chars().any(|c| c.is_whitespace() || c.is_control())
                || u["https://".len().min(u.len())..].contains('@')
            {
                return Err(Error::invalid(format!(
                    "previews.status.api_url {u:?}: an http(s) URL without credentials"
                )));
            }
        }
        Ok(())
    }
}

/// `owner/repo` and the host of a git URL (`https://h/o/r.git`,
/// `git@h:o/r.git`, `ssh://git@h:22/o/r`).
pub fn repo_of(git_url: &str) -> Option<(String, String, String)> {
    let (scheme, host, path) = match git_url.split_once("://") {
        Some((scheme, rest)) => {
            let (auth, path) = rest.split_once('/')?;
            let host = auth.rsplit('@').next()?.to_string();
            (scheme.to_string(), host, path.to_string())
        }
        None => {
            let (h, path) = git_url.split_once(':')?;
            let host = h.rsplit('@').next()?.to_string();
            ("ssh".to_string(), host, path.to_string())
        }
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplitn(2, '/');
    let repo = parts.next()?.to_string();
    let owner = parts.next()?.rsplit('/').next()?.to_string();
    if owner.is_empty() || repo.is_empty() || host.is_empty() {
        return None;
    }
    Some((scheme, host, format!("{owner}/{repo}")))
}

/// The API base for `git_url`: `api_url` when set; GitHub's API for
/// github.com; else `https://<host>/api/v1` (Gitea, Forgejo). A plain-HTTP
/// git URL needs an explicit `api_url`: a token is never sent in clear
/// unless someone wrote that down.
pub fn api_base(kind: ForgeKind, git_url: &str, api_url: Option<&str>) -> Result<String> {
    if let Some(u) = api_url {
        return Ok(u.trim_end_matches('/').to_string());
    }
    let (scheme, host, _) = repo_of(git_url)
        .ok_or_else(|| Error::invalid(format!("cannot tell the repository of {git_url}")))?;
    let bare = host.split(':').next().unwrap_or(&host).to_ascii_lowercase();
    match kind {
        ForgeKind::Github if bare == "github.com" => Ok("https://api.github.com".into()),
        ForgeKind::Github => Ok(format!("https://{bare}/api/v3")),
        ForgeKind::Gitea if scheme == "https" => Ok(format!("https://{host}/api/v1")),
        ForgeKind::Gitea if matches!(scheme.as_str(), "ssh" | "git+ssh" | "ssh+git") => {
            Ok(format!("https://{bare}/api/v1"))
        }
        ForgeKind::Gitea => Err(Error::invalid(format!(
            "{git_url} is not HTTPS: set previews.status.api_url to send the token anyway"
        ))),
    }
}

/// A commit status: `pending`, `success`, `failure` or `error`.
pub struct Status<'a> {
    pub state: &'a str,
    pub target_url: Option<&'a str>,
    pub description: &'a str,
}

/// Post one commit status for `sha` of `owner_repo`.
pub fn post_status(
    kind: ForgeKind,
    api: &str,
    owner_repo: &str,
    token: &str,
    sha: &str,
    st: &Status,
) -> Result<()> {
    if !crate::app::git::is_sha(sha) {
        return Err(Error::invalid(format!("not a commit SHA: {sha}")));
    }
    let (owner, repo) = owner_repo
        .split_once('/')
        .ok_or_else(|| Error::invalid(format!("not owner/repo: {owner_repo}")))?;
    let url = format!(
        "{api}/repos/{}/{}/statuses/{sha}",
        crate::client::encode_segment(owner),
        crate::client::encode_segment(repo)
    );
    let step = format!("post a commit status to {api}");
    let fail = |m: String| Error::OperationFailed {
        step: step.clone(),
        message: m,
    };
    let auth = match kind {
        ForgeKind::Github => format!("Bearer {}", token.trim()),
        ForgeKind::Gitea => format!("token {}", token.trim()),
    };
    if auth.contains(['\n', '\r']) {
        return Err(Error::invalid("the forge token holds a line break"));
    }
    let mut desc: String = st.description.chars().take(139).collect();
    if desc.is_empty() {
        desc = st.state.into();
    }
    let mut body = json!({"state": st.state, "description": desc, "context": CONTEXT});
    if let Some(u) = st.target_url {
        body["target_url"] = json!(u);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let payload = serde_json::to_vec(&body)?;
    let mut resp = agent
        .post(&url)
        .header("Authorization", &auth)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .send(&payload[..])
        .map_err(|e| fail(e.to_string()))?;
    let code = resp.status().as_u16();
    if (200..300).contains(&code) {
        return Ok(());
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(64 << 10)
        .read_to_string()
        .unwrap_or_default();
    let first: String = text
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    Err(fail(format!("HTTP {code}: {first}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    #[test]
    fn repos_and_api_bases() {
        assert_eq!(
            repo_of("https://github.com/acme/web.git").unwrap(),
            ("https".into(), "github.com".into(), "acme/web".into())
        );
        assert_eq!(
            repo_of("git@github.com:acme/web.git").unwrap().2,
            "acme/web"
        );
        assert_eq!(
            repo_of("ssh://git@git.example.com:2222/team/sub/app")
                .unwrap()
                .2,
            "sub/app"
        );
        assert!(repo_of("https://h/onlyone").is_none());
        let gh = |u| api_base(ForgeKind::Github, u, None).unwrap();
        assert_eq!(gh("https://github.com/a/b"), "https://api.github.com");
        assert_eq!(gh("git@github.com:a/b"), "https://api.github.com");
        assert_eq!(gh("https://ghe.corp/a/b"), "https://ghe.corp/api/v3");
        let gt = |u| api_base(ForgeKind::Gitea, u, None);
        assert_eq!(
            gt("https://git.example.com:3000/a/b").unwrap(),
            "https://git.example.com:3000/api/v1"
        );
        assert!(gt("http://10.0.0.2:3000/a/b").is_err(), "no token in clear");
        assert_eq!(
            api_base(
                ForgeKind::Gitea,
                "http://10.0.0.2:3000/a/b",
                Some("http://10.0.0.2:3000/api/v1/")
            )
            .unwrap(),
            "http://10.0.0.2:3000/api/v1"
        );
        let s = |u: &str| StatusSettings {
            token_secret: "t".into(),
            kind: None,
            api_url: Some(u.into()),
        };
        assert!(s("https://api.github.com").validate().is_ok());
        assert!(s("https://u:p@h/api").validate().is_err());
        assert!(s("file:///x").validate().is_err());
    }

    /// A one-request HTTP server: returns what it received.
    fn fake(
        status: u16,
    ) -> (
        String,
        std::thread::JoinHandle<(String, Vec<String>, String)>,
    ) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut headers = Vec::new();
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end().to_string();
                if h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                headers.push(h);
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let mut s = s;
            write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
            (line, headers, String::from_utf8(body).unwrap())
        });
        (addr, h)
    }

    #[test]
    fn posts_github_and_gitea_shapes() {
        let sha = "a".repeat(40);
        let st = Status {
            state: "success",
            target_url: Some("https://web-x.sslip.io/"),
            description: "preview ready",
        };
        let (api, h) = fake(201);
        post_status(ForgeKind::Github, &api, "acme/web", "tok\n", &sha, &st).unwrap();
        let (line, headers, body) = h.join().unwrap();
        assert_eq!(
            line.trim_end(),
            format!("POST /repos/acme/web/statuses/{sha} HTTP/1.1")
        );
        assert!(
            headers
                .iter()
                .any(|h| h == "authorization: Bearer tok" || h == "Authorization: Bearer tok"),
            "{headers:?}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["state"], "success");
        assert_eq!(v["context"], CONTEXT);
        assert_eq!(v["target_url"], "https://web-x.sslip.io/");

        let (api, h) = fake(201);
        post_status(
            ForgeKind::Gitea,
            &format!("{api}/api/v1"),
            "o/r",
            "t2",
            &sha,
            &st,
        )
        .unwrap();
        let (line, headers, _) = h.join().unwrap();
        assert!(line.starts_with(&format!("POST /api/v1/repos/o/r/statuses/{sha} ")));
        assert!(
            headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case("authorization: token t2"))
        );

        let (api, h) = fake(403);
        let e = post_status(ForgeKind::Github, &api, "o/r", "t", &sha, &st).unwrap_err();
        assert!(e.to_string().contains("403"), "{e}");
        h.join().unwrap();
        assert!(post_status(ForgeKind::Github, "http://x", "o/r", "t", "main", &st).is_err());
    }
}
