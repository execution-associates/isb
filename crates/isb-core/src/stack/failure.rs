//! Why a replica did not come up.
//!
//! A replica that fails its rollout is deleted at once and the controller
//! tries again, so by the time anyone asks for its logs the instance is
//! gone (or dying, and incus answers the console request with an error).
//! The controller therefore reads the failed instance's output before it
//! deletes it, puts the last lines in the failure message (the deployment
//! log, the service's status) and keeps them here for `app_logs`, which
//! shows them beside the live replicas' logs.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

use super::controller::{Inst, list_instances};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::sandbox::Sandbox;
use crate::supervise;

/// How many lines of a failed replica's output are read.
const READ_LINES: usize = 60;
/// How many of them go into the failure message.
const MESSAGE_LINES: usize = 8;
/// The most characters the message takes from them.
const MESSAGE_CHARS: usize = 1200;

/// The first wait after a replica failed because its image is missing.
pub const IMAGE_RETRY: Duration = Duration::from_secs(300);
/// The longest wait between such attempts.
pub const IMAGE_RETRY_MAX: Duration = Duration::from_secs(3600);

/// Why a replica's image could not be pulled, in plain words, when that is
/// why it failed: `image docker:x:1 not found (manifest unknown)`. `None`
/// for any other failure, and for a registry that did not answer (that
/// may pass by itself).
pub fn image_missing(image: &str, err: &str) -> Option<String> {
    if !err.contains("Failed getting remote image") && !err.contains("Error parsing image name") {
        return None;
    }
    match crate::image_check::classify(err) {
        crate::image_check::Probe::NotFound(why) => {
            Some(format!("image {image} not found ({why})"))
        }
        crate::image_check::Probe::Denied(why) => Some(format!(
            "image {image} cannot be pulled without credentials ({why}): it is private or does not exist"
        )),
        _ => None,
    }
}

/// After a replica could not be created: how long to wait before the next
/// attempt (`prev` was the last wait), and the service's message. An image
/// its registry does not have will not appear by itself (until someone
/// pushes it), so that waits longest, and is said plainly instead of in
/// incus' words.
pub fn retry(prev: Option<Duration>, image: &str, msg: &str, e: &Error) -> (Duration, String) {
    let missing = image_missing(image, &e.to_string());
    let (first, cap) = match missing {
        Some(_) => (IMAGE_RETRY, IMAGE_RETRY_MAX),
        None => (Duration::from_secs(10), Duration::from_secs(300)),
    };
    let wait = prev.map(|w| (w * 2).clamp(first, cap)).unwrap_or(first);
    let message = match missing {
        Some(m) => format!(
            "{m}: change the image and deploy again (retrying in {}m)",
            wait.as_secs() / 60
        ),
        None => format!("{msg}; retrying in {wait:?}"),
    };
    (wait, message)
}

/// The output of the last replica of a service that failed to come up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailedAttempt {
    /// The instance that was deleted.
    pub instance: String,
    /// When it failed, in milliseconds since the epoch.
    pub at_ms: u64,
    /// Why (the failure message without the output).
    pub reason: String,
    /// The last lines it printed.
    pub output: String,
}

/// The last failed attempt of each (stack, service).
#[derive(Default)]
pub struct Failures(Mutex<BTreeMap<(String, String), FailedAttempt>>);

impl Failures {
    pub fn record(&self, stack: &str, service: &str, a: FailedAttempt) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((stack.to_string(), service.to_string()), a);
    }

    pub fn last(&self, stack: &str, service: &str) -> Option<FailedAttempt> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(stack.to_string(), service.to_string()))
            .cloned()
    }

    /// A service that came up again has nothing to explain.
    pub fn clear(&self, stack: &str, service: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(stack.to_string(), service.to_string()));
    }
}

/// The last `n` non-empty lines of `text`, joined with ` | ` and cut to
/// `max` characters (from the front: the end is what explains it).
pub fn one_line(text: &str, n: usize, max: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let joined = lines[lines.len().saturating_sub(n)..].join(" | ");
    let count = joined.chars().count();
    if count <= max {
        return joined;
    }
    let tail: String = joined.chars().skip(count - max).collect();
    format!("...{tail}")
}

/// How long a failed instance's output is waited for. incus gives the
/// console of a container that has just exited only after a moment: an
/// early read is an error or empty.
const OUTPUT_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(8)
};

/// Poll `read` until it yields text, for at most `within`. An empty string
/// when the instance printed nothing or cannot be read: the failure is
/// explained without it.
fn poll_output(
    read: &mut dyn FnMut() -> Result<String>,
    within: Duration,
    poll: Duration,
) -> String {
    let until = std::time::Instant::now() + within;
    loop {
        if let Ok(t) = read() {
            if !t.trim().is_empty() {
                return t;
            }
        }
        if std::time::Instant::now() >= until {
            return String::new();
        }
        std::thread::sleep(poll);
    }
}

/// The failed instance's output, read before it is deleted.
fn read_output(client: &Client, name: &str, service: &str, oci: bool) -> String {
    poll_output(
        &mut || {
            let sb = Sandbox::get(client, name)?;
            supervise::logs(&sb, service, oci, READ_LINES)
        },
        OUTPUT_WAIT,
        Duration::from_millis(500),
    )
}

/// The failure of replica `name` with its last output in the message, and
/// the attempt to keep. Called before the instance is deleted.
pub fn explain(
    client: &Client,
    name: &str,
    service: &str,
    oci: bool,
    e: Error,
    now_ms: u64,
) -> (Error, FailedAttempt) {
    let output = read_output(client, name, service, oci);
    let reason = e.to_string();
    let tail = one_line(&output, MESSAGE_LINES, MESSAGE_CHARS);
    let err = if tail.is_empty() {
        e
    } else {
        Error::invalid(format!("{reason}; its last output: {tail}"))
    };
    let attempt = FailedAttempt {
        instance: name.to_string(),
        at_ms: now_ms,
        reason,
        output,
    };
    (err, attempt)
}

/// Recent output of a service's replicas (or one slot's), by instance. A
/// replica that is being replaced as this is asked, or whose output cannot
/// be read, shows why in place of its text instead of failing the call.
pub fn replica_logs(
    client: &Client,
    stack: &str,
    service: &str,
    oci: bool,
    slot: Option<u32>,
    lines: usize,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for i in list_instances(client, stack, Some(service))? {
        if slot.is_some_and(|s| s != i.slot) {
            continue;
        }
        out.insert(i.name.clone(), one_replica(client, &i, service, oci, lines));
    }
    Ok(out)
}

fn one_replica(client: &Client, i: &Inst, service: &str, oci: bool, lines: usize) -> String {
    let mut last = String::new();
    for attempt in 0..2 {
        match Sandbox::get(client, &i.name).and_then(|sb| supervise::logs(&sb, service, oci, lines))
        {
            Ok(t) => return t,
            Err(e) if e.is_not_found() => return "(replaced while reading its logs)".into(),
            Err(e) => last = e.to_string(),
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    format!("(no logs: {last})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_image_backs_off_long_and_says_so() {
        let e = Error::invalid(
            "create instance x failed: Failed getting remote image info: Failed to run: skopeo inspect docker://docker.io/library/traefik:whoami: reading manifest whoami in docker.io/library/traefik: manifest unknown",
        );
        let (wait, m) = retry(None, "docker:traefik:whoami", "slot 1: ...", &e);
        assert_eq!(wait, Duration::from_secs(300));
        assert_eq!(
            m,
            "image docker:traefik:whoami not found (manifest unknown): change the image and deploy again (retrying in 5m)"
        );
        let mut w = Some(Duration::from_secs(10));
        for _ in 0..8 {
            w = Some(retry(w, "docker:traefik:whoami", "slot 1: ...", &e).0);
        }
        assert_eq!(w, Some(Duration::from_secs(3600)));
        // Anything else starts at seconds.
        let (wait, m) = retry(
            None,
            "docker:nginx",
            "slot 1: boom",
            &Error::invalid("boom"),
        );
        assert_eq!(wait, Duration::from_secs(10));
        assert_eq!(m, "slot 1: boom; retrying in 10s");
    }

    #[test]
    fn a_pull_of_a_missing_image_is_said_plainly() {
        let incus = r#"create instance web-1 failed: Failed getting remote image info: Failed to run: skopeo --insecure-policy inspect docker://docker.io/library/traefik:whoami --no-tags: exit status 2 (time="2026-10-05T04:38:08Z" level=fatal msg="Error parsing image name \"docker://docker.io/library/traefik:whoami\": reading manifest whoami in docker.io/library/traefik: manifest unknown")"#;
        assert_eq!(
            image_missing("docker:traefik:whoami", incus).as_deref(),
            Some("image docker:traefik:whoami not found (manifest unknown)")
        );
        let offline = "create instance web-1 failed: Failed getting remote image info: Failed to run: skopeo: dial tcp: lookup registry-1.docker.io: no such host";
        assert_eq!(image_missing("docker:nginx", offline), None);
        assert_eq!(
            image_missing("docker:nginx", "out of disk: manifest unknown"),
            None
        );
    }

    #[test]
    fn the_message_carries_the_end_of_the_output() {
        let out = "starting\n\nconnecting to db\nError: getaddrinfo ENOTFOUND umami-db\n   at GetAddrInfoReqWrap\n";
        assert_eq!(
            one_line(out, 2, 500),
            "Error: getaddrinfo ENOTFOUND umami-db |    at GetAddrInfoReqWrap"
        );
        assert_eq!(one_line("", 8, 100), "");
        assert_eq!(one_line("a\nb\nc\n", 8, 100), "a | b | c");
        // Cut from the front, keeping the last characters.
        let long = format!("{}\nENOTFOUND", "x".repeat(300));
        let s = one_line(&long, 8, 40);
        assert!(s.starts_with("...") && s.ends_with("ENOTFOUND"), "{s}");
        assert_eq!(s.chars().count(), 43);
    }

    #[test]
    fn the_last_attempt_is_kept_per_service_until_it_serves_again() {
        let f = Failures::default();
        let a = FailedAttempt {
            instance: "shop-web-1-abc".into(),
            at_ms: 7,
            reason: "failed within the 5s monitor period".into(),
            output: "boom".into(),
        };
        assert_eq!(f.last("shop", "web"), None);
        f.record("shop", "web", a.clone());
        assert_eq!(f.last("shop", "web"), Some(a));
        assert_eq!(f.last("shop", "db"), None);
        f.clear("shop", "web");
        assert_eq!(f.last("shop", "web"), None);
    }

    #[test]
    fn an_unreadable_instance_still_explains_the_failure_without_output() {
        // No incusd behind this socket: reading the output fails, and the
        // original failure is returned unchanged.
        let c = Client::with_socket("/nonexistent/incus.sock");
        let (e, a) = explain(
            &c,
            "gone",
            "web",
            true,
            Error::invalid("failed within the 5s monitor period"),
            9,
        );
        assert_eq!(e.to_string(), "failed within the 5s monitor period");
        assert_eq!(a.reason, "failed within the 5s monitor period");
        assert!(a.output.is_empty());
        assert_eq!((a.instance.as_str(), a.at_ms), ("gone", 9));
    }

    #[test]
    fn a_replica_deleted_while_its_logs_are_read_does_not_fail_the_call() {
        use crate::client::fake::{Route, serve};
        use crate::stack::{LABEL_SERVICE, LABEL_SLOT, LABEL_STACK};
        // The instance is listed, then gone (the crash loop replaced it).
        let (_d, c) = serve(vec![Route {
            prefix: "GET /1.0/instances?recursion=1",
            status: 200,
            body: serde_json::json!([{
                "name": "web-1-aaaa",
                "status": "Running",
                "config": {
                    format!("user.{LABEL_STACK}"): "shop",
                    format!("user.{LABEL_SERVICE}"): "web",
                    format!("user.{LABEL_SLOT}"): "1",
                },
            }]),
        }]);
        let logs = replica_logs(&c, "shop", "web", true, None, 50).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs["web-1-aaaa"], "(replaced while reading its logs)");
        // Another slot's logs were asked for: nothing is read.
        assert!(
            replica_logs(&c, "shop", "web", true, Some(2), 50)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn output_that_arrives_late_is_waited_for_and_silence_ends() {
        let mut n = 0;
        let got = poll_output(
            &mut || {
                n += 1;
                match n {
                    1 => Err(Error::invalid("connection refused")),
                    2 => Ok("\n".into()),
                    _ => Ok("Error: getaddrinfo ENOTFOUND db\n".into()),
                }
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(got, "Error: getaddrinfo ENOTFOUND db\n");
        assert_eq!(n, 3);
        // An instance that prints nothing is not waited on for long.
        let t = std::time::Instant::now();
        let none = poll_output(
            &mut || Ok(String::new()),
            Duration::from_millis(30),
            Duration::from_millis(10),
        );
        assert_eq!(none, "");
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
