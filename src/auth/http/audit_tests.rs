//! Every auth event leaves one audit row, with who, how and how it went,
//! and never a password or a token.

use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::audit::{AuditLog, Entry, Query, Visibility};

const PW: &str = "correct horse battery";
const PW2: &str = "another horse battery";

struct T {
    api: Arc<AuthApi>,
    router: Router,
    log: Arc<AuditLog>,
    _dir: tempfile::TempDir,
}

fn api() -> T {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(
        AuditLog::open(
            &dir.path().join("audit.db"),
            crate::audit::DEFAULT_RETENTION,
        )
        .unwrap(),
    );
    let (s, _) = crate::auth::tests::store_with(crate::auth::tests::fast_config());
    let api = Arc::new(
        AuthApi::new(
            Arc::new(s),
            ApiConfig {
                audit: Some(log.clone()),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    T {
        router: api.clone().router(),
        api,
        log,
        _dir: dir,
    }
}

impl T {
    fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, Value, Response) {
        let mut h: Vec<(String, String)> = vec![
            ("Host".into(), "localhost:8092".into()),
            ("X-Isb-Csrf".into(), "1".into()),
            ("User-Agent".into(), "audit-test/1".into()),
        ];
        h.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let r = Request {
            method: method.into(),
            path: format!("{PREFIX}{path}"),
            query: None,
            headers: h,
            body: body
                .map(|b| serde_json::to_vec(&b).unwrap())
                .unwrap_or_default(),
            peer: Peer::Tcp("127.0.0.1:40000".parse::<SocketAddr>().unwrap()),
        };
        let resp = (self.router)(&r).unwrap();
        let v = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
        (resp.status, v, resp)
    }

    /// The rows appended since `after`, oldest first.
    fn rows(&self, after: i64) -> Vec<Entry> {
        self.log
            .list(
                &Query {
                    after: Some(after),
                    limit: Some(1000),
                    ..Default::default()
                },
                &Visibility::All,
            )
            .unwrap()
    }

    /// Run `f`, then expect exactly one new row, and return it.
    fn one(&self, f: impl FnOnce()) -> Entry {
        let before = self.log.head().unwrap();
        f();
        let rows = self.rows(before);
        assert_eq!(
            rows.len(),
            1,
            "{rows:#?} head {before} -> {} all {:?}",
            self.log.head().unwrap(),
            self.log
                .list(&Query::default(), &Visibility::All)
                .map(|v| v.len())
        );
        rows.into_iter().next().unwrap()
    }
}

fn cookie(r: &Response) -> String {
    r.get_header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn auth_events_are_audited() {
    let t = api();
    // First-run setup.
    let mut root = String::new();
    let e = t.one(|| {
        let token = t.api.setup_token().unwrap();
        let (st, _, r) = t.call(
            "POST",
            "setup",
            &[],
            Some(json!({"setup_token": token, "email": "root@x.io", "password": PW})),
        );
        assert_eq!(st, 201);
        root = cookie(&r);
    });
    assert_eq!(
        (e.action.as_str(), e.actor.as_str(), e.outcome.as_str()),
        ("auth.setup", "root@x.io", "ok")
    );
    assert_eq!(
        (e.surface.as_str(), e.actor_kind.as_str()),
        ("web", "person")
    );
    assert_eq!(e.user_agent.as_deref(), Some("audit-test/1"));
    assert_eq!(e.ip.as_deref(), Some("127.0.0.1"));
    let root_c = [("Cookie", root.as_str())];

    // A failed sign-in names the address tried, as anonymous.
    let e = t.one(|| {
        t.call(
            "POST",
            "login",
            &[],
            Some(json!({"email": "Root@x.io", "password": "wrong password!"})),
        );
    });
    assert_eq!(
        (e.action.as_str(), e.actor.as_str(), e.actor_kind.as_str()),
        ("auth.login", "root@x.io", "anonymous")
    );
    assert_eq!(e.outcome, "invalid_credentials");
    assert_eq!(e.user_id, None);
    let e = t.one(|| {
        t.call(
            "POST",
            "login",
            &[],
            Some(json!({"email": "root@x.io", "password": PW})),
        );
    });
    assert_eq!((e.outcome.as_str(), e.user_id), ("ok", Some(1)));
    assert_eq!(e.details["method"], "password");

    // Tokens: made in an org, used, revoked.
    let mut token = String::new();
    let mut tid = 0;
    let e = t.one(|| {
        let (st, v, _) = t.call(
            "POST",
            "tokens",
            &root_c,
            Some(json!({"name": "ci", "org": "default", "scopes": ["deploy"]})),
        );
        assert_eq!(st, 201, "{v}");
        token = v["token"].as_str().unwrap().into();
        tid = v["info"]["id"].as_i64().unwrap();
    });
    assert_eq!(
        (
            e.action.as_str(),
            e.org.as_deref(),
            e.target.as_deref(),
            e.outcome.as_str()
        ),
        ("auth.token_create", Some("default"), Some("ci"), "ok")
    );
    assert_eq!(e.details["id"], tid);
    // A deploy-scoped token cannot change accounts: refused, and recorded.
    let bearer = format!("Bearer {token}");
    let e = t.one(|| {
        let (st, _, _) = t.call(
            "POST",
            "tokens",
            &[("Authorization", &bearer)],
            Some(json!({"name": "more", "org": "default"})),
        );
        assert_eq!(st, 403);
    });
    assert_eq!(
        (
            e.action.as_str(),
            e.outcome.as_str(),
            e.actor_kind.as_str(),
            e.surface.as_str()
        ),
        ("auth.token_create", "forbidden", "agent", "rest")
    );
    assert_eq!(
        (e.token_id, e.token_name.as_deref()),
        (Some(tid), Some("ci"))
    );
    let e = t.one(|| {
        let (st, _, _) = t.call("DELETE", &format!("tokens/{tid}"), &root_c, None);
        assert_eq!(st, 204);
    });
    assert_eq!(
        (e.action.as_str(), e.org.as_deref(), e.target),
        ("auth.token_revoke", Some("default"), Some(tid.to_string()))
    );

    // Invitations: made, accepted (a new account), revoked.
    let mut inv = String::new();
    let e = t.one(|| {
        let (st, v, _) = t.call(
            "POST",
            "invitations",
            &root_c,
            Some(json!({"org": "ocai", "email": "m@x.io", "role": "viewer"})),
        );
        assert_eq!(st, 201, "{v}");
        inv = v["token"].as_str().unwrap().into();
    });
    assert_eq!(
        (e.action.as_str(), e.org.as_deref(), e.target.as_deref()),
        ("auth.invitation_create", Some("ocai"), Some("m@x.io"))
    );
    assert_eq!(e.details["role"], "viewer");
    let mut member = String::new();
    let e = t.one(|| {
        let (st, v, r) = t.call(
            "POST",
            "invitations/accept",
            &[],
            Some(json!({"token": inv, "name": "M", "password": PW})),
        );
        assert_eq!(st, 200, "{v}");
        member = cookie(&r);
    });
    assert_eq!(
        (e.action.as_str(), e.org.as_deref(), e.actor.as_str()),
        ("auth.invitation_accept", Some("ocai"), "m@x.io")
    );
    let mid = e.user_id.unwrap();
    let (_, v, _) = t.call(
        "POST",
        "invitations",
        &root_c,
        Some(json!({"org": "ocai", "email": "z@x.io"})),
    );
    let iid = v["invitation"]["id"].as_i64().unwrap();
    let e = t.one(|| {
        t.call(
            "DELETE",
            &format!("orgs/ocai/invitations/{iid}"),
            &root_c,
            None,
        );
    });
    assert_eq!(e.action, "auth.invitation_revoke");

    // Roles: changed, member removed.
    let e = t.one(|| {
        let (st, _, _) = t.call(
            "PUT",
            &format!("orgs/ocai/members/{mid}"),
            &root_c,
            Some(json!({"role": "member"})),
        );
        assert_eq!(st, 200);
    });
    assert_eq!(
        (
            e.action.as_str(),
            e.org.as_deref(),
            e.details["role"].as_str()
        ),
        ("auth.role_change", Some("ocai"), Some("member"))
    );

    // Password change, then reset (request and confirm).
    let mc = [("Cookie", member.as_str())];
    let e = t.one(|| {
        let (st, _, _) = t.call(
            "POST",
            "password",
            &mc,
            Some(json!({"current_password": PW, "new_password": PW2})),
        );
        assert_eq!(st, 204);
    });
    assert_eq!(
        (e.action.as_str(), e.actor.as_str()),
        ("auth.password_change", "m@x.io")
    );
    let e = t.one(|| {
        t.call(
            "POST",
            "password-reset/request",
            &[],
            Some(json!({"email": "m@x.io"})),
        );
    });
    assert_eq!(e.action, "auth.password_reset_request");
    let reset = t
        .api
        .store
        .request_password_reset("m@x.io")
        .unwrap()
        .unwrap();
    let e = t.one(|| {
        let (st, _, _) = t.call(
            "POST",
            "password-reset/confirm",
            &[],
            Some(json!({"token": reset, "password": PW})),
        );
        assert_eq!(st, 204);
    });
    assert_eq!(
        (e.action.as_str(), e.user_id),
        ("auth.password_reset", Some(mid))
    );

    // Identities and passkeys.
    let member = {
        let (_, _, r) = t.call(
            "POST",
            "login",
            &[],
            Some(json!({"email": "m@x.io", "password": PW})),
        );
        cookie(&r)
    };
    let mc = [("Cookie", member.as_str())];
    let ident = t
        .api
        .store
        .link_identity(
            mid,
            &crate::auth::external::ExternalIdentity {
                provider: "github".into(),
                subject: "42".into(),
                email: Some("m@x.io".into()),
                email_verified: true,
                name: None,
            },
        )
        .unwrap();
    let e = t.one(|| {
        let (st, _, _) = t.call("DELETE", &format!("identities/{}", ident.id), &mc, None);
        assert_eq!(st, 204);
    });
    assert_eq!(
        (e.action.as_str(), e.target),
        ("auth.identity_unlink", Some(ident.id.to_string()))
    );
    let e = t.one(|| {
        t.call("DELETE", "passkeys/99", &mc, None);
    });
    assert_eq!(
        (e.action.as_str(), e.outcome.as_str()),
        ("auth.passkey_remove", "not_found")
    );

    // SSH keys: added with the fingerprint as the target, listed, removed.
    let key =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh m@laptop";
    let mut kid = 0;
    let e = t.one(|| {
        let (st, v, _) = t.call("POST", "ssh-keys", &mc, Some(json!({"public_key": key})));
        assert_eq!(st, 201, "{v}");
        assert_eq!(v["ssh_key"]["name"], "m@laptop");
        kid = v["ssh_key"]["id"].as_i64().unwrap();
    });
    assert_eq!(
        (e.action.as_str(), e.target.as_deref(), e.outcome.as_str()),
        (
            "auth.ssh_key_add",
            Some("SHA256:OGav3hSvMQSOfHiDB0OdyYFOPHDbUJTzSsNvCdLadvQ"),
            "ok"
        )
    );
    assert_eq!(e.details["kind"], "ssh-ed25519");
    // Options smuggled in front of a key: refused, and recorded.
    let e = t.one(|| {
        let (st, _, _) = t.call(
            "POST",
            "ssh-keys",
            &mc,
            Some(json!({"public_key": format!("command=\"sh\" {key}")})),
        );
        assert_eq!(st, 400);
    });
    assert_eq!(
        (e.action.as_str(), e.outcome.as_str()),
        ("auth.ssh_key_add", "invalid")
    );
    let (st, v, _) = t.call("GET", "ssh-keys", &mc, None);
    assert_eq!((st, v["ssh_keys"].as_array().map(Vec::len)), (200, Some(1)));
    // Another account cannot remove it.
    let (st, _, _) = t.call("DELETE", &format!("ssh-keys/{kid}"), &root_c, None);
    assert_eq!(st, 404);
    let e = t.one(|| {
        let (st, _, _) = t.call("DELETE", &format!("ssh-keys/{kid}"), &mc, None);
        assert_eq!(st, 204);
    });
    assert_eq!(
        (e.action.as_str(), e.target),
        ("auth.ssh_key_remove", Some(kid.to_string()))
    );

    // Platform administration: one row per change.
    let before = t.log.head().unwrap();
    let (st, _, _) = t.call(
        "PATCH",
        &format!("admin/users/{mid}"),
        &root_c,
        Some(json!({"disabled": true, "platform_admin": true})),
    );
    assert_eq!(st, 200);
    let actions: Vec<String> = t.rows(before).into_iter().map(|e| e.action).collect();
    assert_eq!(actions, ["auth.user_disable", "auth.platform_admin_grant"]);

    let e = t.one(|| {
        t.call("DELETE", &format!("orgs/ocai/members/{mid}"), &root_c, None);
    });
    assert_eq!(e.action, "auth.member_remove");
    let e = t.one(|| {
        t.call("POST", "logout", &root_c, None);
    });
    assert_eq!(
        (e.action.as_str(), e.actor.as_str()),
        ("auth.logout", "root@x.io")
    );
    // Reads are not recorded.
    let e0 = t.log.head().unwrap();
    t.call("GET", "me", &[("Cookie", &member)], None);
    assert_eq!(t.log.head().unwrap(), e0);

    // Never a password or a token, anywhere in the file.
    assert!(t.log.verify().unwrap().ok);
    let path = t.log.path().unwrap().to_path_buf();
    let mut bytes = std::fs::read(&path).unwrap();
    if let Ok(w) = std::fs::read(path.with_extension("db-wal")) {
        bytes.extend(w);
    }
    for s in [
        PW,
        PW2,
        "wrong password!",
        token.as_str(),
        inv.as_str(),
        reset.as_str(),
    ] {
        assert!(
            !bytes.windows(s.len()).any(|w| w == s.as_bytes()),
            "{s:?} is in the audit log"
        );
    }
}
