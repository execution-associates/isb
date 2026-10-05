#[test]
fn a_reveal_argument_makes_a_secret_read() {
    let t = Tool {
        name: "database_get".into(),
        title: None,
        description: String::new(),
        input_schema: serde_json::json!({"type": "object"}),
        annotations: Some(serde_json::json!({"readOnlyHint": true, "isbSecretReadArg": "reveal"})),
        handler: std::sync::Arc::new(|_, _| Ok(Value::Null)),
    };
    let plain = class_for(&t, &serde_json::json!({"name": "pg"}));
    assert!(plain.read_only && !plain.secret_read);
    let reveal = class_for(&t, &serde_json::json!({"name": "pg", "reveal": true}));
    assert!(!reveal.read_only && reveal.secret_read);
    assert!(scope_allows(&["read".to_string()], "database_get", plain));
    assert!(!scope_allows(&["read".to_string()], "database_get", reveal));
}
use super::*;
use crate::auth::Role;
use crate::server::ToolPolicy;
use crate::server::http::Peer;
use crate::server::mcp::{Authenticated, Endpoint, Hooks};
use std::collections::HashMap;

const SECRET: &str = "hunter2-very-secret-value";

struct T {
    ep: Endpoint,
    log: Arc<AuditLog>,
    _dir: tempfile::TempDir,
}

/// An endpoint with the daemon's authorizer and audit hook over a few
/// stand-in tools, and callers picked by `Authorization: Bearer NAME`.
fn endpoint() -> T {
    use super::super::tests::{token, user};
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(
        AuditLog::open(
            &dir.path().join("audit.db"),
            crate::audit::DEFAULT_RETENTION,
        )
        .unwrap(),
    );
    let mut r = Registry::new();
    let ro = json!({"readOnlyHint": true});
    let write = json!({"destructiveHint": false});
    for (name, ann) in [
        ("secret_set", &write),
        ("stack_deploy", &write),
        ("sandbox_exec", &write),
        ("stack_status", &ro),
        ("secret_get", &ro),
    ] {
        r.register(
            Tool::new(name, "", json!({}), |a: Value, _c: &Caller| {
                Ok(json!({"echo": a.get("name"), "value": "c2VjcmV0"}))
            })
            .annotations(ann.clone()),
        )
        .unwrap();
    }
    register(&mut r, log.clone()).unwrap();
    let callers: HashMap<&str, Caller> = HashMap::from([
        ("agent", token(&[("acme", Role::Member)], &[])),
        ("ro", token(&[("acme", Role::Member)], &["read"])),
        ("viewer", user(&[("acme", Role::Viewer)], false)),
        ("admin", user(&[("acme", Role::Admin)], false)),
        ("member", user(&[("acme", Role::Member)], false)),
        ("beta", user(&[("beta", Role::Owner)], false)),
        ("root", user(&[], true)),
    ]);
    let authn: crate::server::mcp::Authn = Arc::new(move |req, _| {
        let Some(a) = req.header("authorization") else {
            return Authenticated::None;
        };
        match callers.get(a.trim_start_matches("Bearer ")) {
            Some(Caller::User { principal }) => Authenticated::User(principal.clone()),
            _ => Authenticated::Refused,
        }
    });
    let ep = Endpoint {
        registry: Arc::new(r),
        policy: ToolPolicy::default(),
        access: None,
        healthz: Arc::new(|| (true, json!({}))),
        routes: None,
        public_routes: None,
        hooks: Hooks {
            authn: Some(authn),
            authorize: Some(Arc::new(|c, tool, args, scope| {
                super::super::authorize_class(
                    c,
                    &tool.name,
                    class_for(tool, &args),
                    args,
                    scope,
                    false,
                )
            })),
            events: None,
            terminal: None,
            ssh: None,
            audit: Some(hook(log.clone(), false)),
            ..Default::default()
        },
    };
    T { ep, log, _dir: dir }
}

impl T {
    fn rest(&self, who: Option<&str>, tool: &str, args: Value) -> u16 {
        let mut headers = vec![("User-Agent".to_string(), "t/1".to_string())];
        if let Some(w) = who {
            headers.push(("Authorization".into(), format!("Bearer {w}")));
        }
        let peer = match who {
            Some(_) => Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
            None => Peer::Unix { uid: Some(1000) },
        };
        let r = self.ep.handle(&crate::server::http::Request {
            method: "POST".into(),
            path: format!("/api/v1/tools/{tool}"),
            query: None,
            headers,
            body: serde_json::to_vec(&args).unwrap(),
            peer,
        });
        r.status
    }

    fn mcp(&self, who: &str, tool: &str, args: Value) -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": args}});
        let r = self.ep.handle(&crate::server::http::Request {
            method: "POST".into(),
            path: "/mcp".into(),
            query: None,
            headers: vec![("Authorization".into(), format!("Bearer {who}"))],
            body: serde_json::to_vec(&body).unwrap(),
            peer: Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
        });
        serde_json::from_slice(&r.body).unwrap()
    }

    fn new_rows(&self, after: i64) -> Vec<crate::audit::Entry> {
        self.log
            .list(
                &Query {
                    after: Some(after),
                    ..Default::default()
                },
                &Visibility::All,
            )
            .unwrap()
    }

    fn one(&self, f: impl FnOnce()) -> crate::audit::Entry {
        let h = self.log.head().unwrap();
        f();
        let mut rows = self.new_rows(h);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        rows.remove(0)
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn tool_calls_are_audited_without_values() {
    let t = endpoint();
    // A write by an agent's token over REST.
    let e = t.one(|| {
        let st = t.rest(
            Some("agent"),
            "secret_set",
            json!({"org": "acme", "name": "DB_PASSWORD", "value": SECRET}),
        );
        assert_eq!(st, 200);
    });
    assert_eq!(
        (e.action.as_str(), e.org.as_deref(), e.target.as_deref()),
        ("secret_set", Some("acme"), Some("DB_PASSWORD"))
    );
    assert_eq!(
        (e.actor.as_str(), e.actor_kind.as_str(), e.surface.as_str()),
        ("u@x.io", "agent", "rest")
    );
    assert_eq!(
        (e.user_id, e.token_id, e.token_name.as_deref()),
        (Some(7), Some(3), Some("ci"))
    );
    assert_eq!(
        (e.outcome.as_str(), e.user_agent.as_deref()),
        ("ok", Some("t/1"))
    );
    assert!(e.request_id.is_some());
    assert_eq!(e.details, json!({"org": "acme", "name": "DB_PASSWORD"}));
    // A deploy with secrets in every place they can hide.
    let e = t.one(|| {
        let r = t.mcp(
            "agent",
            "stack_deploy",
            json!({
                "org": "acme", "name": "web",
                "compose": format!("services: {{web: {{environment: {{P: {SECRET}}}}}}}"),
                "secrets": {"db": SECRET}, "vars": {"X": SECRET},
                "env": {"K": SECRET}, "argv": ["echo", SECRET], "stdin": SECRET,
                "service": SECRET.repeat(10),
            }),
        );
        assert_eq!(r["result"]["isError"], false);
    });
    assert_eq!(
        (e.action.as_str(), e.surface.as_str()),
        ("stack_deploy", "mcp")
    );
    assert_eq!(e.details, json!({"org": "acme", "name": "web"}));
    // A secret read is recorded; a plain read is not.
    let e = t.one(|| {
        let st = t.rest(
            Some("member"),
            "secret_get",
            json!({"org": "acme", "name": "DB_PASSWORD"}),
        );
        assert_eq!(st, 200);
    });
    assert_eq!(
        (e.action.as_str(), e.actor_kind.as_str()),
        ("secret_get", "person")
    );
    let h = t.log.head().unwrap();
    let st = t.rest(
        Some("member"),
        "stack_status",
        json!({"org": "acme", "name": "web"}),
    );
    assert_eq!(st, 200);
    assert!(t.new_rows(h).is_empty());
    // Refusals are recorded with their code: a viewer writing, a read
    // token reading a secret, someone outside the org.
    for (who, tool) in [
        ("viewer", "stack_deploy"),
        ("viewer", "secret_get"),
        ("ro", "sandbox_exec"),
        ("beta", "secret_set"),
    ] {
        let e = t.one(|| {
            let st = t.rest(
                Some(who),
                tool,
                json!({"org": "acme", "name": "x", "value": SECRET}),
            );
            assert_eq!(st, 403);
        });
        assert_eq!(
            (e.action.as_str(), e.outcome.as_str()),
            (tool, "forbidden"),
            "{who}"
        );
    }
    // A refusal on an org-bound endpoint is filed under that org.
    let e = t.one(|| {
        let r = t.ep.handle(&crate::server::http::Request {
            method: "POST".into(),
            path: "/orgs/acme/api/v1/tools/secret_get".into(),
            query: None,
            headers: vec![("Authorization".into(), "Bearer viewer".into())],
            body: br#"{"name": "DB_PASSWORD"}"#.to_vec(),
            peer: Peer::Tcp("127.0.0.1:5000".parse().unwrap()),
        });
        assert_eq!(r.status, 403);
    });
    assert_eq!(
        (e.org.as_deref(), e.outcome.as_str()),
        (Some("acme"), "forbidden")
    );
    // The local CLI over the socket.
    let e = t.one(|| {
        let st = t.rest(None, "stack_deploy", json!({"org": "acme", "name": "api"}));
        assert_eq!(st, 200);
    });
    assert_eq!(
        (e.actor.as_str(), e.actor_kind.as_str(), e.surface.as_str()),
        ("local(uid 1000)", "local", "cli")
    );
    // Not a byte of any value in the database or its WAL.
    assert!(t.log.verify().unwrap().ok);
    let p = t.log.path().unwrap().to_path_buf();
    let mut bytes = std::fs::read(&p).unwrap();
    if let Ok(w) = std::fs::read(p.with_extension("db-wal")) {
        bytes.extend(w);
    }
    for needle in [SECRET, "c2VjcmV0", "hunter2"] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "{needle} leaked"
        );
    }
}

#[test]
fn audit_list_shows_each_reader_their_share() {
    let t = endpoint();
    t.rest(
        Some("agent"),
        "secret_set",
        json!({"org": "acme", "name": "a"}),
    );
    t.rest(
        Some("beta"),
        "secret_set",
        json!({"org": "beta", "name": "b"}),
    );
    t.log
        .append(NewEntry {
            actor: Actor::claimed("x@y.io"),
            action: "auth.login".into(),
            outcome: "invalid_credentials".into(),
            ..Default::default()
        })
        .unwrap();
    let list = |who: &str, args: Value| t.mcp(who, "audit_list", args)["result"].clone();
    let orgs = |v: &Value| -> Vec<Option<String>> {
        v["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["org"].as_str().map(String::from))
            .collect()
    };
    let acme = vec![Some("acme".to_string())];
    // An org admin: their org only, with or without naming it.
    assert_eq!(orgs(&list("admin", json!({}))), acme);
    assert_eq!(orgs(&list("admin", json!({"org": "acme"}))), acme);
    assert_eq!(list("admin", json!({"org": "beta"}))["isError"], true);
    assert_eq!(list("admin", json!({"platform": true}))["isError"], true);
    // Members, viewers and tokens below admin: refused.
    for who in ["member", "viewer", "agent"] {
        let r = list(who, json!({"org": "acme"}));
        assert_eq!(r["isError"], true, "{who}");
    }
    // A platform admin: everything, platform-level rows included.
    let all = orgs(&list("root", json!({})));
    assert!(
        all.contains(&None) && all.contains(&Some("beta".into())),
        "{all:?}"
    );
    assert_eq!(orgs(&list("root", json!({"platform": true}))), [None]);
    // audit_verify is for platform admins.
    let r = t.mcp("admin", "audit_verify", json!({}));
    assert_eq!(r["result"]["isError"], true);
    let v = t.mcp("root", "audit_verify", json!({}));
    assert_eq!(v["result"]["structuredContent"]["ok"], true);
}

#[test]
fn ssh_sessions_are_recorded_with_user_and_key() {
    use super::super::tests::token;
    let c = token(&[("acme", Role::Member)], &[]);
    let args = json!({
        "org": "acme", "name": "box", "ssh_user": "dev",
        "fingerprint": "SHA256:OGav3hSvMQSOfHiDB0OdyYFOPHDbUJTzSsNvCdLadvQ",
        "duration_s": 42,
    });
    let origin = Origin::default();
    for action in ["ssh.open", "ssh.close"] {
        let e = entry(
            &Audited {
                caller: &c,
                action,
                tool: None,
                args: &args,
                outcome: Ok(()),
                origin: &origin,
            },
            false,
        )
        .expect("SSH sessions are always recorded");
        assert_eq!(e.org.as_deref(), Some("acme"));
        assert_eq!(e.target.as_deref(), Some("box"));
        assert_eq!(e.details["ssh_user"], "dev");
        assert_eq!(e.details["duration_s"], 42);
        assert!(
            e.details["fingerprint"]
                .as_str()
                .unwrap()
                .starts_with("SHA256:")
        );
    }
}
