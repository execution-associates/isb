use std::net::SocketAddr;

use serde_json::{Value, json};

use super::*;
use crate::auth::PrincipalKind;
use crate::auth::tests::{fast_config, store};

const PW: &str = "correct horse battery";

struct T {
    api: Arc<AuthApi>,
    router: Router,
}

fn api() -> T {
    api_with(fast_config())
}

fn api_with(cfg: crate::auth::AuthConfig) -> T {
    let (s, _) = crate::auth::tests::store_with(cfg);
    let api = Arc::new(AuthApi::new(Arc::new(s), ApiConfig::default()).unwrap());
    T {
        router: api.clone().router(),
        api,
    }
}

/// A request from loopback to `localhost:8092`, plain HTTP, like the
/// local web UI in development.
fn req(method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Request {
    req_from("127.0.0.1:40000", method, path, headers, body)
}

fn req_from(
    peer: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Request {
    let mut h: Vec<(String, String)> = vec![("Host".into(), "localhost:8092".into())];
    h.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Request {
        method: method.into(),
        path: path.into(),
        query: None,
        headers: h,
        body: body
            .map(|b| serde_json::to_vec(&b).unwrap())
            .unwrap_or_default(),
        peer: Peer::Tcp(peer.parse::<SocketAddr>().unwrap()),
    }
}

const CSRF: (&str, &str) = ("X-Isb-Csrf", "1");

impl T {
    fn call(&self, r: Request) -> (u16, Value, Response) {
        let resp = (self.router)(&r).expect("an auth path");
        let v = if resp.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&resp.body).unwrap()
        };
        (resp.status, v, resp)
    }

    fn post(&self, path: &str, headers: &[(&str, &str)], body: Value) -> (u16, Value, Response) {
        let mut h = vec![CSRF];
        h.extend_from_slice(headers);
        self.call(req("POST", &format!("{PREFIX}{path}"), &h, Some(body)))
    }

    fn get(&self, path: &str, headers: &[(&str, &str)]) -> (u16, Value, Response) {
        self.call(req("GET", &format!("{PREFIX}{path}"), headers, None))
    }

    /// Run setup over HTTP; returns the admin's cookie header value.
    fn setup(&self) -> String {
        let token = self.api.setup_token().unwrap();
        let (st, v, r) = self.post(
            "setup",
            &[],
            json!({"setup_token": token, "email": "root@x.io", "name": "Root", "password": PW}),
        );
        assert_eq!(st, 201, "{v}");
        cookie_of(&r)
    }

    fn login(&self, email: &str) -> String {
        let (st, v, r) = self.post("login", &[], json!({"email": email, "password": PW}));
        assert_eq!(st, 200, "{v}");
        cookie_of(&r)
    }
}

/// `isb_session=...` from a response's Set-Cookie, ready for a Cookie header.
fn cookie_of(r: &Response) -> String {
    r.get_header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn only_auth_paths_are_claimed() {
    let t = api();
    assert!((t.router)(&req("GET", "/mcp", &[], None)).is_none());
    assert!((t.router)(&req("GET", "/api/v1/other", &[], None)).is_none());
    assert!((t.router)(&req("GET", "/api/v1/authx", &[], None)).is_none());
    let (st, v, r) = t.get("nope", &[]);
    assert_eq!((st, v["error"].as_str()), (404, Some("not_found")));
    assert_eq!(r.get_header("cache-control"), Some("no-store"));
    let (st, _, r) = t.get("login", &[]);
    assert_eq!(st, 405);
    assert_eq!(r.get_header("allow"), Some("POST"));
}

#[test]
fn setup_needs_the_setup_token_once() {
    let t = api();
    let (st, v, _) = t.get("setup", &[]);
    assert_eq!((st, v["needed"].as_bool()), (200, Some(true)));
    // Without the CSRF header: refused before anything else.
    let (st, v, _) = t.call(req(
        "POST",
        "/api/v1/auth/setup",
        &[],
        Some(
            json!({"setup_token": t.api.setup_token().unwrap(), "email": "a@x.io", "password": PW}),
        ),
    ));
    assert_eq!((st, v["error"].as_str()), (403, Some("csrf")));
    // A wrong setup token.
    let (fake, _) = secret::new_token(TokenKind::Setup).unwrap();
    let (st, v, _) = t.post(
        "setup",
        &[],
        json!({"setup_token": fake, "email": "a@x.io", "password": PW}),
    );
    assert_eq!((st, v["error"].as_str()), (400, Some("invalid_token")));
    let cookie = t.setup();
    assert!(cookie.starts_with("isb_session=isb_sess_"));
    let (st, v, _) = t.get("setup", &[]);
    assert_eq!((st, v["needed"].as_bool()), (200, Some(false)));
    let (st, _, _) = t.post(
        "setup",
        &[],
        json!({"setup_token": t.api.setup_token().unwrap(), "email": "b@x.io", "password": PW}),
    );
    assert_eq!(st, 409);
    // The setup session is a platform admin.
    let (st, v, _) = t.get("me", &[("Cookie", &cookie)]);
    assert_eq!(st, 200);
    assert_eq!(v["platform_admin"], true);
    assert_eq!(
        v["memberships"][0],
        json!({"org": "default", "role": "owner"})
    );
    // A platform admin sees every org, members or not.
    t.api
        .store
        .ensure_org(&OrgId::new("zeta").unwrap())
        .unwrap();
    let (_, v, _) = t.get("me", &[("Cookie", &cookie)]);
    assert_eq!(v["orgs"], json!(["default", "zeta"]));
}

#[test]
fn setup_token_file_is_written_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("setup-token");
    let (s, _) = store();
    let s = Arc::new(s);
    let api = AuthApi::new(
        s.clone(),
        ApiConfig {
            setup_token_file: Some(f.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let token = std::fs::read_to_string(&f).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(token.trim(), api.setup_token().unwrap());
    // Setup done another way (the CLI): the file goes on the next look.
    s.create_first_admin("a@x.io", "A", PW).unwrap();
    let r = api
        .handle(&req("GET", "/api/v1/auth/setup", &[], None))
        .unwrap();
    assert_eq!(r.status, 200);
    assert!(!f.exists());
}

#[test]
fn cookie_flags() {
    let t = api_with(crate::auth::AuthConfig {
        login_per_email: crate::auth::limit::Rate::new(100, 1),
        ..fast_config()
    });
    t.setup();
    let login = |headers: &[(&str, &str)], host: &str| {
        let mut r = req(
            "POST",
            "/api/v1/auth/login",
            &[CSRF],
            Some(json!({"email": "root@x.io", "password": PW})),
        );
        r.headers.retain(|(k, _)| k != "Host");
        r.headers.push(("Host".into(), host.into()));
        r.headers
            .extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let resp = (t.router)(&r).unwrap();
        assert_eq!(resp.status, 200);
        resp.get_header("set-cookie").unwrap().to_string()
    };
    // Plain loopback HTTP: no Secure, or the browser would drop it.
    let c = login(&[], "localhost:8092");
    assert!(c.starts_with("isb_session=isb_sess_"), "{c}");
    for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(c.contains(flag), "{flag} in {c}");
    }
    assert!(!c.contains("Secure"), "{c}");
    assert!(!login(&[], "127.0.0.1:8092").contains("Secure"));
    assert!(!login(&[], "[::1]:8092").contains("Secure"));
    // Through a tunnel or a TLS proxy: Secure.
    assert!(login(&[("X-Forwarded-Proto", "https")], "localhost:8092").contains("; Secure"));
    assert!(login(&[], "isb.example.com").contains("; Secure"));
    assert!(login(&[("Cf-Visitor", r#"{"scheme":"https"}"#)], "localhost").contains("; Secure"));
}

#[test]
fn login_failures_look_the_same() {
    let t = api();
    t.setup();
    let wrong = t.post(
        "login",
        &[],
        json!({"email": "root@x.io", "password": "wrong password!"}),
    );
    let unknown = t.post(
        "login",
        &[],
        json!({"email": "nobody@x.io", "password": PW}),
    );
    assert_eq!(wrong.0, 401);
    assert_eq!(wrong.0, unknown.0);
    assert_eq!(wrong.2.body, unknown.2.body);
    assert_eq!(wrong.1["error"], "invalid_credentials");
    assert!(wrong.2.get_header("set-cookie").is_none());
    // Malformed bodies are a 400, not a crash.
    let (st, _, _) = t.post("login", &[], json!({"email": 1}));
    assert_eq!(st, 400);
}

#[test]
fn login_rate_limit_is_a_429_with_retry_after() {
    let t = api();
    t.setup();
    for _ in 0..5 {
        let (st, _, _) = t.post(
            "login",
            &[],
            json!({"email": "root@x.io", "password": "nope nope nope"}),
        );
        assert_eq!(st, 401);
    }
    let (st, v, r) = t.post("login", &[], json!({"email": "root@x.io", "password": PW}));
    assert_eq!((st, v["error"].as_str()), (429, Some("rate_limited")));
    assert!(r.get_header("retry-after").unwrap().parse::<u64>().unwrap() > 0);
}

#[test]
fn rate_limits_use_the_tunnel_client_address() {
    let r = req("GET", "/", &[("Cf-Connecting-IP", "203.0.113.9")], None);
    assert_eq!(client_ip(&r).as_deref(), Some("203.0.113.9"));
    let r = req("GET", "/", &[("Cf-Connecting-IP", "not an ip")], None);
    assert_eq!(client_ip(&r).as_deref(), Some("127.0.0.1"));
    // Only a loopback peer (the tunnel) is believed.
    let r = req_from(
        "192.0.2.1:5",
        "GET",
        "/",
        &[("Cf-Connecting-IP", "203.0.113.9")],
        None,
    );
    assert_eq!(client_ip(&r).as_deref(), Some("192.0.2.1"));
}

#[test]
fn me_logout_and_csrf() {
    let t = api();
    let cookie = t.setup();
    let (st, v, _) = t.get("me", &[]);
    assert_eq!((st, v["error"].as_str()), (401, Some("unauthenticated")));
    let (st, v, _) = t.get("me", &[("Cookie", &format!("theme=dark; {cookie}"))]);
    assert_eq!(st, 200);
    assert_eq!(v["user"]["email"], "root@x.io");
    assert_eq!(v["auth"]["kind"], "session");
    assert!(v["user"].get("password_hash").is_none());
    // A cookie-authenticated state change without the header is refused, and
    // the session survives it.
    let (st, v, _) = t.call(req(
        "POST",
        "/api/v1/auth/logout",
        &[("Cookie", &cookie)],
        None,
    ));
    assert_eq!((st, v["error"].as_str()), (403, Some("csrf")));
    let (st, _, _) = t.call(req(
        "POST",
        "/api/v1/auth/logout",
        &[("Cookie", &cookie), ("X-Isb-Csrf", "yes")],
        None,
    ));
    assert_eq!(st, 403);
    assert_eq!(t.get("me", &[("Cookie", &cookie)]).0, 200);
    // With it, logout ends the session and clears the cookie.
    let (st, _, r) = t.post("logout", &[("Cookie", &cookie)], Value::Null);
    assert_eq!(st, 204);
    let c = r.get_header("set-cookie").unwrap();
    assert!(
        c.starts_with("isb_session=;") && c.contains("Max-Age=0"),
        "{c}"
    );
    assert_eq!(t.get("me", &[("Cookie", &cookie)]).0, 401);
    // Sessions list and revoke.
    let c1 = t.login("root@x.io");
    let c2 = t.login("root@x.io");
    let (_, v, _) = t.get("sessions", &[("Cookie", &c1)]);
    let list = v["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let other = list.iter().find(|s| s["current"] == false).unwrap();
    let id = other["id"].as_i64().unwrap();
    let (st, _, _) = t.call(req(
        "DELETE",
        &format!("{PREFIX}sessions/{id}"),
        &[CSRF, ("Cookie", &c1)],
        None,
    ));
    assert_eq!(st, 204);
    assert_eq!(t.get("me", &[("Cookie", &c2)]).0, 401);
    assert_eq!(t.get("me", &[("Cookie", &c1)]).0, 200);
}

#[test]
fn tokens_over_http() {
    let t = api();
    let cookie = t.setup();
    let (st, v, _) = t.post(
        "tokens",
        &[("Cookie", &cookie)],
        json!({"name": "ci", "org": "default", "expires": "90d"}),
    );
    assert_eq!(st, 201, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("isb_tok_"));
    assert_eq!(v["info"]["org"], "default");
    let bearer = format!("Bearer {token}");
    // A bearer token authenticates without cookies, and needs no CSRF header.
    let (st, v, _) = t.get("me", &[("Authorization", &bearer)]);
    assert_eq!(st, 200);
    assert_eq!(
        v["auth"],
        json!({"kind": "api_token", "id": 1, "org": "default", "name": "ci"})
    );
    assert_eq!(v["platform_admin"], false);
    // An org token sees only its org.
    assert_eq!(v["orgs"], json!(["default"]));
    let (st, v, _) = t.call(req(
        "POST",
        "/api/v1/auth/tokens",
        &[("Authorization", &bearer)],
        Some(json!({"name": "child", "org": "default"})),
    ));
    // A token cannot mint tokens, even for its own org with its own reach
    // (a copy would outlive the token's revocation).
    assert_eq!(st, 403, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("cannot mint"),
        "{v}"
    );
    // Nor a platform token, nor one for another org.
    let (st, _, _) = t.call(req(
        "POST",
        "/api/v1/auth/tokens",
        &[("Authorization", &bearer)],
        Some(json!({"name": "escalate"})),
    ));
    assert_eq!(st, 403);
    let (st, _, _) = t.call(req(
        "POST",
        "/api/v1/auth/tokens",
        &[("Authorization", &bearer)],
        Some(json!({"name": "escalate", "org": "ocai"})),
    ));
    assert_eq!(st, 403);
    // A bad bearer never falls back to the cookie.
    let (st, _, _) = t.get(
        "me",
        &[
            ("Authorization", "Bearer isb_tok_wrong"),
            ("Cookie", &cookie),
        ],
    );
    assert_eq!(st, 401);
    let (st, _, _) = t.get("me", &[("Authorization", "Basic abc"), ("Cookie", &cookie)]);
    assert_eq!(st, 401);
    // List and revoke.
    let (_, v, _) = t.get("tokens", &[("Cookie", &cookie)]);
    assert_eq!(v["tokens"].as_array().unwrap().len(), 1);
    let (st, _, _) = t.call(req(
        "DELETE",
        "/api/v1/auth/tokens/1",
        &[CSRF, ("Cookie", &cookie)],
        None,
    ));
    assert_eq!(st, 204);
    assert_eq!(t.get("me", &[("Authorization", &bearer)]).0, 401);
    // Expiry must parse.
    let (st, _, _) = t.post(
        "tokens",
        &[("Cookie", &cookie)],
        json!({"name": "x", "expires": "soon"}),
    );
    assert_eq!(st, 400);
    // Changing a password needs a session, not a token.
    let (_, v, _) = t.post("tokens", &[("Cookie", &cookie)], json!({"name": "p"}));
    let b = format!("Bearer {}", v["token"].as_str().unwrap());
    let (st, _, _) = t.call(req(
        "POST",
        "/api/v1/auth/password",
        &[("Authorization", &b)],
        Some(json!({"current_password": PW, "new_password": "brand new password"})),
    ));
    assert_eq!(st, 403);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn invitations_over_http() {
    let t = api();
    let root = t.setup();
    // Root (platform admin) invites an owner of a new org.
    let (st, v, _) = t.post(
        "invitations",
        &[("Cookie", &root)],
        json!({"org": "ocai", "email": "Owner@X.io", "role": "owner"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["invitation"]["email"], "owner@x.io");
    assert!(v["link"].is_null(), "no public URL configured");
    let token = v["token"].as_str().unwrap().to_string();
    let (st, v, _) = t.post("invitations/inspect", &[], json!({"token": token}));
    assert_eq!(st, 200);
    assert_eq!(v["account_exists"], false);
    assert_eq!(v["role"], "owner");
    // Accepting creates the account and signs in.
    let (st, v, r) = t.post(
        "invitations/accept",
        &[],
        json!({"token": token, "name": "Olive", "password": PW}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["created"], true);
    let owner = cookie_of(&r);
    let (_, me, _) = t.get("me", &[("Cookie", &owner)]);
    assert_eq!(me["memberships"], json!([{"org": "ocai", "role": "owner"}]));
    let (st, _, _) = t.post(
        "invitations/accept",
        &[],
        json!({"token": token, "password": PW}),
    );
    assert_eq!(st, 400);

    // The owner invites a member; the member cannot invite.
    let (_, v, _) = t.post(
        "invitations",
        &[("Cookie", &owner)],
        json!({"org": "ocai", "email": "m@x.io"}),
    );
    assert_eq!(v["invitation"]["role"], "member");
    let tok = v["token"].as_str().unwrap();
    let (_, _, r) = t.post(
        "invitations/accept",
        &[],
        json!({"token": tok, "name": "Mo", "password": PW}),
    );
    let member = cookie_of(&r);
    let (st, _, _) = t.post(
        "invitations",
        &[("Cookie", &member)],
        json!({"org": "ocai", "email": "z@x.io"}),
    );
    assert_eq!(st, 403);
    // Nor can anyone invite into an org they are not in.
    let (st, _, _) = t.post(
        "invitations",
        &[("Cookie", &owner)],
        json!({"org": "default", "email": "z@x.io"}),
    );
    assert_eq!(st, 403);

    // Promote the member to admin; an admin cannot invite an owner.
    let (_, members, _) = t.get("orgs/ocai/members", &[("Cookie", &member)]);
    let mid = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user"]["email"] == "m@x.io")
        .unwrap()["user"]["id"]
        .as_i64()
        .unwrap();
    let put = |cookie: &str, uid: i64, role: &str| {
        t.call(req(
            "PUT",
            &format!("{PREFIX}orgs/ocai/members/{uid}"),
            &[CSRF, ("Cookie", cookie)],
            Some(json!({"role": role})),
        ))
        .0
    };
    assert_eq!(put(&member, mid, "admin"), 403);
    assert_eq!(put(&owner, mid, "admin"), 200);
    let (st, _, _) = t.post(
        "invitations",
        &[("Cookie", &member)],
        json!({"org": "ocai", "email": "z@x.io", "role": "owner"}),
    );
    assert_eq!(st, 403);
    let (st, v, _) = t.post(
        "invitations",
        &[("Cookie", &member)],
        json!({"org": "ocai", "email": "z@x.io", "role": "admin"}),
    );
    assert_eq!(st, 201);
    let inv_id = v["invitation"]["id"].as_i64().unwrap();
    let (_, v, _) = t.get("orgs/ocai/invitations", &[("Cookie", &member)]);
    assert_eq!(v["invitations"].as_array().unwrap().len(), 1);
    let (st, _, _) = t.call(req(
        "DELETE",
        &format!("{PREFIX}orgs/ocai/invitations/{inv_id}"),
        &[CSRF, ("Cookie", &member)],
        None,
    ));
    assert_eq!(st, 204);
    // An admin cannot demote the owner; the last owner cannot be demoted.
    let (_, me, _) = t.get("me", &[("Cookie", &owner)]);
    let oid = me["user"]["id"].as_i64().unwrap();
    assert_eq!(put(&member, oid, "member"), 403);
    assert_eq!(put(&owner, oid, "admin"), 409);
    // An outsider sees no org at all.
    let (st, _, _) = t.get("orgs/default/members", &[("Cookie", &owner)]);
    assert_eq!(st, 404);
    // Leaving.
    let (st, _, _) = t.call(req(
        "DELETE",
        &format!("{PREFIX}orgs/ocai/members/{mid}"),
        &[CSRF, ("Cookie", &member)],
        None,
    ));
    assert_eq!(st, 204);
    assert_eq!(t.get("orgs/ocai/members", &[("Cookie", &member)]).0, 404);
}

#[test]
fn accept_as_the_signed_in_user() {
    let t = api();
    let root = t.setup();
    let (_, v, _) = t.post(
        "invitations",
        &[("Cookie", &root)],
        json!({"org": "ocai", "email": "root@x.io", "role": "admin"}),
    );
    let (st, v, r) = t.post(
        "invitations/accept",
        &[("Cookie", &root)],
        json!({"token": v["token"]}),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["membership"], json!({"org": "ocai", "role": "admin"}));
    assert!(r.get_header("set-cookie").is_none());
}

#[test]
fn links_use_the_public_url() {
    let (s, _) = store();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent2 = sent.clone();
    let api = Arc::new(
        AuthApi::new(
            Arc::new(s),
            ApiConfig {
                public_url: Some("https://isb.example.com/".into()),
                notifier: Some(Arc::new(move |n: &Notice| {
                    sent2.lock().unwrap().push(n.clone());
                    Ok(())
                })),
                setup_token_file: None,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let t = T {
        router: api.clone().router(),
        api,
    };
    let root = t.setup();
    let (_, v, _) = t.post(
        "invitations",
        &[("Cookie", &root)],
        json!({"org": "ocai", "email": "a@x.io"}),
    );
    let link = v["link"].as_str().unwrap();
    assert_eq!(
        link,
        format!(
            "https://isb.example.com/invite#{}",
            v["token"].as_str().unwrap()
        )
    );
    // Reset requests answer the same either way; the notifier gets the link.
    let known = t.post("password-reset/request", &[], json!({"email": "root@x.io"}));
    let unknown = t.post("password-reset/request", &[], json!({"email": "who@x.io"}));
    assert_eq!(known.0, 202);
    assert_eq!((known.0, &known.2.body), (unknown.0, &unknown.2.body));
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let Notice::PasswordReset { email, token, link } = &sent[0];
    assert_eq!(email, "root@x.io");
    assert_eq!(
        link.as_deref(),
        Some(format!("https://isb.example.com/reset-password#{token}").as_str())
    );
    // Confirming sets the password and ends the sessions.
    let (st, _, _) = t.post(
        "password-reset/confirm",
        &[],
        json!({"token": token, "password": "brand new password"}),
    );
    assert_eq!(st, 204);
    assert_eq!(t.get("me", &[("Cookie", &root)]).0, 401);
    let (st, _, _) = t.post(
        "login",
        &[],
        json!({"email": "root@x.io", "password": "brand new password"}),
    );
    assert_eq!(st, 200);
    let (st, v, _) = t.post(
        "password-reset/confirm",
        &[],
        json!({"token": token, "password": "another password!"}),
    );
    assert_eq!((st, v["error"].as_str()), (400, Some("invalid_token")));
}

#[test]
fn change_password_over_http() {
    let t = api();
    let a = t.setup();
    let b = t.login("root@x.io");
    let (st, _, _) = t.post(
        "password",
        &[("Cookie", &a)],
        json!({"current_password": "wrong wrong wrong", "new_password": "brand new password"}),
    );
    assert_eq!(st, 403);
    let (st, _, _) = t.post(
        "password",
        &[("Cookie", &a)],
        json!({"current_password": PW, "new_password": "brand new password"}),
    );
    assert_eq!(st, 204);
    assert_eq!(t.get("me", &[("Cookie", &a)]).0, 200);
    assert_eq!(t.get("me", &[("Cookie", &b)]).0, 401);
}

#[test]
fn principal_from_request() {
    let (s, _) = crate::auth::tests::store_with(fast_config());
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let n = s.start_session(u.id, LoginMeta::default()).unwrap();
    let t = s.create_api_token(u.id, None, "t", None).unwrap();
    let c = format!("{COOKIE}={}", n.token);
    let p = s
        .principal_from_request(&req("GET", "/x", &[("Cookie", &c)], None))
        .unwrap();
    assert_eq!(p.kind, PrincipalKind::Session { id: n.session.id });
    let b = format!("bearer {}", t.token);
    let p = s
        .principal_from_request(&req("GET", "/x", &[("Authorization", &b)], None))
        .unwrap();
    assert!(matches!(p.kind, PrincipalKind::ApiToken { org: None, .. }));
    assert!(
        s.principal_from_request(&req("GET", "/x", &[], None))
            .is_none()
    );
    // A session token is not a bearer token.
    let b = format!("Bearer {}", n.token);
    assert!(
        s.principal_from_request(&req("GET", "/x", &[("Authorization", &b)], None))
            .is_none()
    );
}

#[test]
fn cookie_parsing() {
    let r = req(
        "GET",
        "/",
        &[("Cookie", "a=1; isb_session=\"tok\""), ("cookie", "b=2")],
        None,
    );
    assert_eq!(cookie(&r, COOKIE).as_deref(), Some("tok"));
    assert_eq!(cookie(&r, "b").as_deref(), Some("2"));
    assert_eq!(cookie(&r, "c"), None);
    let r = req("GET", "/", &[("Cookie", "isb_session=")], None);
    assert_eq!(cookie(&r, COOKIE), None);
}

#[test]
fn admin_users_over_http() {
    let t = api();
    let root = t.setup();
    let (_, v, _) = t.post(
        "invitations",
        &[("Cookie", &root)],
        json!({"org": "ocai", "email": "o@x.io", "role": "owner"}),
    );
    let (_, v, r) = t.post(
        "invitations/accept",
        &[],
        json!({"token": v["token"], "name": "O", "password": PW}),
    );
    let owner = cookie_of(&r);
    let oid = v["user"]["id"].as_i64().unwrap();
    let patch = |cookie: &str, uid: i64, body: Value| {
        t.call(req(
            "PATCH",
            &format!("{PREFIX}admin/users/{uid}"),
            &[CSRF, ("Cookie", cookie)],
            Some(body),
        ))
    };

    // Org owners are not platform admins: refused both ways.
    assert_eq!(t.get("admin/users", &[("Cookie", &owner)]).0, 403);
    assert_eq!(patch(&owner, oid, json!({"platform_admin": true})).0, 403);

    // The list carries memberships and last activity, never hashes.
    let (st, v, _) = t.get("admin/users", &[("Cookie", &root)]);
    assert_eq!(st, 200, "{v}");
    let users = v["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    let o = users.iter().find(|u| u["email"] == "o@x.io").unwrap();
    assert_eq!(o["memberships"], json!([{"org": "ocai", "role": "owner"}]));
    assert!(o["last_active"].as_i64().is_some());
    assert!(!v.to_string().contains("argon2"));

    // Root cannot demote or disable itself.
    let (_, me, _) = t.get("me", &[("Cookie", &root)]);
    let rid = me["user"]["id"].as_i64().unwrap();
    assert_eq!(patch(&root, rid, json!({"disabled": true})).0, 403);
    assert_eq!(patch(&root, rid, json!({"platform_admin": false})).0, 403);
    assert_eq!(patch(&root, rid, json!({"bogus": 1})).0, 400);

    // Disabling ends the user's sessions; enabling lets them sign in again.
    let (st, v, _) = patch(&root, oid, json!({"disabled": true}));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["user"]["disabled"], true);
    assert_eq!(t.get("me", &[("Cookie", &owner)]).0, 401);
    assert_eq!(patch(&root, oid, json!({"disabled": false})).0, 200);
    let owner = t.login("o@x.io");

    // A second platform admin may then demote the first.
    assert_eq!(patch(&root, oid, json!({"platform_admin": true})).0, 200);
    let (_, me, _) = t.get("me", &[("Cookie", &owner)]);
    assert_eq!(me["platform_admin"], true);
    assert_eq!(patch(&owner, rid, json!({"platform_admin": false})).0, 200);
    let (st, v, _) = patch(&root, oid, json!({"platform_admin": false}));
    assert_eq!(st, 403, "root is no longer a platform admin: {v}");
    assert_eq!(patch(&owner, 9999, json!({"disabled": true})).0, 404);
}

#[test]
fn the_last_platform_admin_stays() {
    let t = api();
    let root = t.setup();
    let (_, me, _) = t.get("me", &[("Cookie", &root)]);
    let rid = me["user"]["id"].as_i64().unwrap();
    // A second admin, disabled: it does not count.
    let other = t
        .api
        .store()
        .create_user("b@x.io", "B", Some(PW), true)
        .unwrap();
    t.api.store().set_disabled(other.id, true).unwrap();
    let (st, v, _) = t.call(req(
        "PATCH",
        &format!("{PREFIX}admin/users/{rid}"),
        &[CSRF, ("Cookie", &root)],
        Some(json!({"platform_admin": false})),
    ));
    // Refused as self-demotion first; the store's count is checked too.
    assert_eq!(st, 403, "{v}");
    assert_eq!(t.api.store().other_platform_admins(rid).unwrap(), 0);
    t.api.store().set_disabled(other.id, false).unwrap();
    assert_eq!(t.api.store().other_platform_admins(rid).unwrap(), 1);
}

#[test]
fn org_members_and_tokens_carry_who_and_when() {
    let t = api();
    let root = t.setup();
    let (_, v, _) = t.post(
        "invitations",
        &[("Cookie", &root)],
        json!({"org": "ocai", "email": "m@x.io"}),
    );
    let (_, _, r) = t.post(
        "invitations/accept",
        &[],
        json!({"token": v["token"], "name": "Mo", "password": PW}),
    );
    let member = cookie_of(&r);
    let (_, v, _) = t.get("orgs/ocai/members", &[("Cookie", &member)]);
    assert!(v["members"][0]["last_active"].as_i64().is_some(), "{v}");
    // The platform admin makes a token in an org it is not a member of; the
    // org's token list names its holder.
    let (st, _, _) = t.post(
        "tokens",
        &[("Cookie", &root)],
        json!({"name": "ci", "org": "ocai"}),
    );
    assert_eq!(st, 201);
    let (st, v, _) = t.get("orgs/ocai/tokens", &[("Cookie", &root)]);
    assert_eq!(st, 200);
    assert_eq!(v["tokens"][0]["user"]["email"], "root@x.io");
    // A member may not list the org's tokens.
    assert_eq!(t.get("orgs/ocai/tokens", &[("Cookie", &member)]).0, 403);
}

#[test]
fn superadmins_sign_in_by_their_source_and_never_mint_superadmin_tokens() {
    let (s, _) = crate::auth::tests::store_with(fast_config());
    let s = Arc::new(s);
    let sa = s.create_superadmin_token("agent", None).unwrap();
    let st = s.clone();
    // As the daemon's gate: the superadmin token, or a "tailnet" peer.
    let gate: SuperadminFn = Arc::new(move |r: &Request| {
        let bearer = r
            .header("authorization")
            .and_then(|a| a.strip_prefix("Bearer "));
        if let Some(t) = bearer {
            return st.authenticate_superadmin_token(t).ok().flatten().map(|i| {
                Arc::new(crate::auth::Superadmin::synthetic(
                    crate::auth::SuperadminSource::Token {
                        id: i.id,
                        name: i.name,
                    },
                ))
            });
        }
        matches!(&r.peer, Peer::Tcp(a) if a.ip().to_string() == "100.64.0.1").then(|| {
            Arc::new(crate::auth::Superadmin::synthetic(
                crate::auth::SuperadminSource::Tailnet {
                    login: "tagged-devices".into(),
                    node: "agent.t.ts.net".into(),
                    tags: vec!["tag:agents".into()],
                },
            ))
        })
    });
    let api = Arc::new(
        AuthApi::new(
            s.clone(),
            ApiConfig {
                superadmin: Some(gate),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let t = T {
        router: api.clone().router(),
        api,
    };
    // A tailnet superadmin is signed in without a session.
    let (st, v, _) = t.call(req_from(
        "100.64.0.1:1",
        "GET",
        "/api/v1/auth/me",
        &[],
        None,
    ));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["superadmin"]["source"], "tailnet:agent.t.ts.net");
    assert_eq!(v["superadmin"]["account"], false);
    assert_eq!(v["platform_admin"], true);
    // Not from elsewhere.
    assert_eq!(t.get("me", &[]).0, 401);
    // Neither it nor a superadmin token mints a superadmin token, or any
    // token without an account.
    let bearer = format!("Bearer {}", sa.token);
    for (peer, h) in [
        ("100.64.0.1:1", vec![CSRF]),
        ("127.0.0.1:1", vec![("Authorization", bearer.as_str())]),
    ] {
        let (st, v, _) = t.call(req_from(
            peer,
            "POST",
            "/api/v1/auth/tokens",
            &h,
            Some(json!({"name": "durable", "superadmin": true})),
        ));
        assert_eq!(st, 403, "{v}");
        assert!(
            v["message"].as_str().unwrap().contains("on the host"),
            "{v}"
        );
        let (st, _, _) = t.call(req_from(
            peer,
            "POST",
            "/api/v1/auth/tokens",
            &h,
            Some(json!({"name": "plain"})),
        ));
        assert_eq!(st, 403);
    }
    // Writes still need the CSRF header from the ambient source.
    let (st, _, _) = t.call(req_from(
        "100.64.0.1:1",
        "POST",
        "/api/v1/auth/tokens",
        &[],
        Some(json!({"name": "x"})),
    ));
    assert_eq!(st, 403);
    // Even a session user asking for one is refused.
    s.create_user("u@x.io", "U", Some(PW), true).unwrap();
    let c = t.login("u@x.io");
    let (st, _, _) = t.post(
        "tokens",
        &[("Cookie", &c)],
        json!({"name": "d", "superadmin": true}),
    );
    assert_eq!(st, 403);
    assert_eq!(s.list_superadmin_tokens().unwrap().len(), 1);
}

mod agent_ui;
