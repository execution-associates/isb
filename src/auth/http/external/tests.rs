//! The OAuth flow against fake providers on loopback (an OIDC provider with
//! discovery, JWKS, token and userinfo; GitHub's token, /user and
//! /user/emails), and the passkey ceremonies with software authenticators.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::super::{ApiConfig, AuthApi, PREFIX, Router};
use crate::auth::oauth::{self, ClientSecret, ProviderConfig};
use crate::auth::oidc::testkit::Signer;
use crate::auth::webauthn::testkit::Authenticator;
use crate::auth::webauthn::{b64, sha256};
use crate::auth::{AuthStore, Role};
use crate::org::OrgId;
use crate::server::http::{HttpListener, HttpServer, Limits, Peer, Request, Response, Shutdown};

const PW: &str = "correct horse battery";
const NOW: i64 = 1_800_000_000;
const PUBLIC: &str = "http://localhost:8092";
const CLIENT: &str = "client-1";
const SECRET: &str = "s3cret value";

/// What the fake provider answers, changed by each test step.
struct FakeState {
    issuer: String,
    signer: Signer,
    sub: String,
    email: Option<String>,
    email_verified: bool,
    /// Put in the ID token; the test copies it from the authorize URL.
    nonce: String,
    /// Expected PKCE challenge (from the authorize URL).
    challenge: String,
    /// GitHub: (email, primary, verified).
    gh_emails: Vec<(String, bool, bool)>,
    /// Requests the token endpoint refused, and why.
    refused: Vec<String>,
}

struct Fake {
    state: Arc<Mutex<FakeState>>,
    url: String,
    stop: Shutdown,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.trigger();
    }
}

fn form_of(r: &Request) -> Vec<(String, String)> {
    oauth::parse_query(&String::from_utf8_lossy(&r.body))
}

fn get<'a>(q: &'a [(String, String)], k: &str) -> Option<&'a str> {
    q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

fn fake(signer: Signer) -> Fake {
    let l = HttpListener::bind_tcp("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let state = Arc::new(Mutex::new(FakeState {
        issuer: url.clone(),
        signer,
        sub: "sub-1".into(),
        email: Some("root@x.io".into()),
        email_verified: true,
        nonce: String::new(),
        challenge: String::new(),
        gh_emails: vec![],
        refused: vec![],
    }));
    let st = state.clone();
    let handler = Arc::new(move |r: &Request| -> Response {
        let mut s = st.lock().unwrap();
        let iss = s.issuer.clone();
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/.well-known/openid-configuration") => Response::json(
                200,
                &json!({
                    "issuer": iss,
                    "authorization_endpoint": format!("{iss}/authorize"),
                    "token_endpoint": format!("{iss}/token"),
                    "jwks_uri": format!("{iss}/jwks"),
                    "userinfo_endpoint": format!("{iss}/userinfo"),
                    "token_endpoint_auth_methods_supported": ["client_secret_basic"],
                }),
            ),
            ("GET", "/jwks") => Response::json(200, &json!({"keys": [s.signer.jwk("k1")]})),
            ("POST", "/token") => {
                let f = form_of(r);
                // client_secret_basic, both halves form-encoded.
                use base64::Engine;
                let want = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(format!(
                        "{}:{}",
                        oauth::enc(CLIENT),
                        oauth::enc(SECRET)
                    ))
                );
                let verifier = get(&f, "code_verifier").unwrap_or("");
                let pkce = b64(&sha256(verifier.as_bytes()));
                let why = if r.header("authorization") != Some(want.as_str()) {
                    Some("client auth")
                } else if get(&f, "grant_type") != Some("authorization_code") {
                    Some("grant_type")
                } else if get(&f, "code") != Some("good-code") {
                    Some("code")
                } else if pkce != s.challenge {
                    Some("pkce")
                } else if get(&f, "redirect_uri")
                    != Some(&format!("{PUBLIC}{PREFIX}oauth/oidc/callback"))
                {
                    Some("redirect_uri")
                } else {
                    None
                };
                if let Some(w) = why {
                    s.refused.push(w.to_string());
                    return Response::json(
                        400,
                        &json!({"error": "invalid_grant", "error_description": w}),
                    );
                }
                let mut claims = json!({"iss": iss, "aud": CLIENT, "sub": s.sub, "exp": NOW + 300,
                    "iat": NOW, "nonce": s.nonce, "name": "Root Person"});
                if let Some(e) = &s.email {
                    claims["email"] = json!(e);
                    claims["email_verified"] = json!(s.email_verified);
                }
                let id_token = s.signer.sign("k1", &claims);
                Response::json(
                    200,
                    &json!({"access_token": "at-1", "token_type": "Bearer", "id_token": id_token}),
                )
            }
            ("GET", "/userinfo") => Response::json(200, &json!({"sub": s.sub})),
            ("POST", "/login/oauth/access_token") => {
                let f = form_of(r);
                let verifier = get(&f, "code_verifier").unwrap_or("");
                let ok = get(&f, "client_id") == Some("gh-client")
                    && get(&f, "client_secret") == Some("gh-secret")
                    && get(&f, "code") == Some("good-code")
                    && b64(&sha256(verifier.as_bytes())) == s.challenge;
                if !ok {
                    s.refused.push("github token".into());
                    // GitHub answers 200 with an error body.
                    return Response::json(200, &json!({"error": "bad_verification_code"}));
                }
                Response::json(
                    200,
                    &json!({"access_token": "gho_x", "token_type": "bearer"}),
                )
            }
            ("GET", "/api/user") if r.header("authorization") == Some("Bearer gho_x") => {
                Response::json(200, &json!({"id": 4242, "login": "gina", "name": null}))
            }
            ("GET", "/api/user/emails") if r.header("authorization") == Some("Bearer gho_x") => {
                let list: Vec<Value> = s
                    .gh_emails
                    .iter()
                    .map(|(e, p, v)| json!({"email": e, "primary": p, "verified": v}))
                    .collect();
                Response::json(200, &Value::Array(list))
            }
            _ => Response::json(404, &json!({"error": "not_found"})),
        }
    });
    let stop = Shutdown::new();
    let srv = HttpServer::new(Limits::default(), stop.clone());
    std::thread::spawn(move || srv.run(l, handler));
    Fake { state, url, stop }
}

struct T {
    store: Arc<AuthStore>,
    router: Router,
    fake: Fake,
    clock: Arc<AtomicI64>,
}

fn setup(signer: Signer, open_signup: bool) -> T {
    let fake = fake(signer);
    let (s, clock) = crate::auth::tests::store();
    let store = Arc::new(s);
    store.create_first_admin("root@x.io", "Root", PW).unwrap();
    let providers = vec![
        ProviderConfig::oidc(
            &fake.url,
            CLIENT,
            ClientSecret::Value(SECRET.into()),
            Some("Test IdP"),
        ),
        ProviderConfig::github_at(
            "gh-client",
            ClientSecret::Value("gh-secret".into()),
            &fake.url,
            &format!("{}/api", fake.url),
        ),
    ];
    let api = Arc::new(
        AuthApi::new(
            store.clone(),
            ApiConfig {
                public_url: Some(PUBLIC.into()),
                providers,
                open_signup,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    T {
        store,
        router: api.router(),
        fake,
        clock,
    }
}

fn req(
    method: &str,
    path_and_query: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Request {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (path_and_query.to_string(), None),
    };
    let mut h: Vec<(String, String)> = vec![("Host".into(), "localhost:8092".into())];
    h.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    if method != "GET" {
        h.push(("X-Isb-Csrf".into(), "1".into()));
    }
    Request {
        method: method.into(),
        path,
        query,
        headers: h,
        body: body
            .map(|b| serde_json::to_vec(&b).unwrap())
            .unwrap_or_default(),
        peer: Peer::Tcp("127.0.0.1:40000".parse::<SocketAddr>().unwrap()),
    }
}

/// `name=value` of the Set-Cookie for `name`, if set.
fn set_cookie(r: &Response, name: &str) -> Option<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.split(';').next().unwrap().to_string())
        .find(|c| c.starts_with(&format!("{name}=")))
}

fn json_of(r: &Response) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

/// Where a redirect goes: `(path, query pairs)`.
fn location(r: &Response) -> (String, Vec<(String, String)>) {
    let l = r.get_header("location").expect("a redirect").to_string();
    match l.split_once('?') {
        Some((p, q)) => (p.to_string(), oauth::parse_query(q)),
        None => (l, vec![]),
    }
}

/// A started flow: the state, and the binding cookie.
struct Started {
    state: String,
    cookie: String,
}

impl T {
    fn call(&self, r: Request) -> Response {
        (self.router)(&r).expect("an auth path")
    }

    /// Start a flow; tell the fake the nonce and PKCE challenge it got.
    fn start(&self, provider: &str, query: &str, headers: &[(&str, &str)]) -> Started {
        let r = self.call(req(
            "GET",
            &format!("{PREFIX}oauth/{provider}/start?{query}"),
            headers,
            None,
        ));
        assert_eq!(r.status, 303, "{}", String::from_utf8_lossy(&r.body));
        let (path, q) = location(&r);
        let mut s = self.fake.state.lock().unwrap();
        let expect = if provider == "github" {
            "/login/oauth/authorize"
        } else {
            "/authorize"
        };
        assert_eq!(path, format!("{}{expect}", self.fake.url));
        assert_eq!(get(&q, "code_challenge_method"), Some("S256"));
        assert_eq!(
            get(&q, "redirect_uri"),
            Some(format!("{PUBLIC}{PREFIX}oauth/{provider}/callback").as_str())
        );
        s.challenge = get(&q, "code_challenge").unwrap().to_string();
        s.nonce = get(&q, "nonce").unwrap_or("").to_string();
        let c = r
            .headers
            .iter()
            .find(|(k, v)| k.eq_ignore_ascii_case("set-cookie") && v.starts_with("isb_oauth="))
            .unwrap();
        assert!(
            c.1.contains("HttpOnly")
                && c.1.contains("SameSite=Lax")
                && c.1.contains("Path=/api/v1/auth/oauth/")
        );
        Started {
            state: get(&q, "state").unwrap().to_string(),
            cookie: set_cookie(&r, "isb_oauth").unwrap(),
        }
    }

    fn callback(&self, provider: &str, state: &str, cookie: &str) -> Response {
        let q = oauth::form(&[("code", "good-code"), ("state", state)]);
        self.call(req(
            "GET",
            &format!("{PREFIX}oauth/{provider}/callback?{q}"),
            &[("Cookie", cookie)],
            None,
        ))
    }

    fn me(&self, session: &str) -> Value {
        let r = self.call(req(
            "GET",
            &format!("{PREFIX}me"),
            &[("Cookie", session)],
            None,
        ));
        assert_eq!(r.status, 200);
        json_of(&r)
    }
}

/// The error code of a failed flow's redirect, and that it set no session.
fn refused(r: &Response) -> String {
    assert_eq!(r.status, 303);
    assert!(
        set_cookie(r, "isb_session").is_none(),
        "no session on failure"
    );
    let (path, q) = location(r);
    assert_eq!(path, "/login");
    get(&q, "error").unwrap().to_string()
}

#[test]
fn providers_are_listed() {
    let t = setup(Signer::es256(), false);
    let r = t.call(req("GET", &format!("{PREFIX}providers"), &[], None));
    let v = json_of(&r);
    assert_eq!(v["providers"][0]["id"], "oidc");
    assert_eq!(v["providers"][0]["label"], "Test IdP");
    assert_eq!(v["providers"][0]["kind"], "oidc");
    assert_eq!(v["providers"][1]["kind"], "oauth2");
    assert_eq!(
        v["providers"][1]["start"],
        "/api/v1/auth/oauth/github/start"
    );
    assert_eq!(v["passkeys"], true);
    assert_eq!(v["open_signup"], false);
}

#[test]
fn oidc_links_by_verified_email_then_signs_in() {
    for signer in [Signer::es256(), Signer::rs256()] {
        let t = setup(signer, false);
        let f = t.start("oidc", "next=/orgs/default?tab=apps", &[]);
        let r = t.callback("oidc", &f.state, &f.cookie);
        assert_eq!(r.status, 303, "{}", String::from_utf8_lossy(&r.body));
        assert_eq!(r.get_header("location"), Some("/orgs/default?tab=apps"));
        let session = set_cookie(&r, "isb_session").expect("signed in");
        assert_eq!(t.me(&session)["user"]["email"], "root@x.io");
        let root = t.store.user_by_email("root@x.io").unwrap().unwrap();
        let ids = t.store.list_identities(root.id).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].provider, format!("oidc:{}", t.fake.url));
        assert_eq!(ids[0].subject, "sub-1");

        // Next time the identity alone signs in, even with another email
        // and none verified. Each flow gets a fresh binding cookie.
        {
            let mut s = t.fake.state.lock().unwrap();
            s.email = None;
        }
        let f2 = t.start("oidc", "", &[("Cookie", &f.cookie)]);
        assert_ne!(f2.cookie, f.cookie);
        let r = t.callback("oidc", &f2.state, &f2.cookie);
        assert_eq!(r.get_header("location"), Some("/"));
        assert!(set_cookie(&r, "isb_session").is_some());
        assert!(t.fake.state.lock().unwrap().refused.is_empty());
    }
}

#[test]
fn oidc_refuses_unverified_and_uninvited() {
    let t = setup(Signer::es256(), false);
    // An unverified email that matches a user: never linked.
    t.fake.state.lock().unwrap().email_verified = false;
    let f = t.start("oidc", "", &[]);
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &f.cookie)),
        "unverified_email"
    );
    let root = t.store.user_by_email("root@x.io").unwrap().unwrap();
    assert!(t.store.list_identities(root.id).unwrap().is_empty());

    // A new verified address without an invitation: closed, and the error
    // page keeps `next`.
    {
        let mut s = t.fake.state.lock().unwrap();
        s.email_verified = true;
        s.email = Some("new@x.io".into());
        s.sub = "sub-new".into();
    }
    let f = t.start("oidc", "next=/welcome", &[]);
    let r = t.callback("oidc", &f.state, &f.cookie);
    assert_eq!(refused(&r), "signup_closed");
    assert_eq!(get(&location(&r).1, "next"), Some("/welcome"));
    assert!(t.store.user_by_email("new@x.io").unwrap().is_none());

    // With an invitation (its token riding the flow, POST start), it works.
    let org = OrgId::new("ocai").unwrap();
    let inv = t
        .store
        .create_invitation(None, &org, "new@x.io", Role::Member)
        .unwrap();
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}oauth/oidc/start"),
        &[],
        Some(json!({"next": "/orgs/ocai", "invite": inv.token})),
    ));
    assert_eq!(r.status, 200);
    let url = json_of(&r)["url"].as_str().unwrap().to_string();
    let q = oauth::parse_query(url.split_once('?').unwrap().1);
    {
        let mut s = t.fake.state.lock().unwrap();
        s.nonce = get(&q, "nonce").unwrap().into();
        s.challenge = get(&q, "code_challenge").unwrap().into();
    }
    let cookie = set_cookie(&r, "isb_oauth").unwrap();
    let r = t.callback("oidc", get(&q, "state").unwrap(), &cookie);
    assert_eq!(r.get_header("location"), Some("/orgs/ocai"));
    let session = set_cookie(&r, "isb_session").unwrap();
    let me = t.me(&session);
    assert_eq!(me["user"]["email"], "new@x.io");
    assert_eq!(me["user"]["has_password"], false);
    assert_eq!(me["memberships"][0]["org"], "ocai");
}

#[test]
fn open_signup_needs_only_a_verified_email() {
    let t = setup(Signer::es256(), true);
    {
        let mut s = t.fake.state.lock().unwrap();
        s.email = Some("walkin@x.io".into());
        s.sub = "walk".into();
    }
    let f = t.start("oidc", "", &[]);
    let r = t.callback("oidc", &f.state, &f.cookie);
    let session = set_cookie(&r, "isb_session").expect("signed up");
    assert_eq!(t.me(&session)["memberships"], json!([]));
}

#[test]
fn next_must_be_a_local_path() {
    let t = setup(Signer::es256(), false);
    for bad in [
        "//evil.example/x",
        "https://evil.example/",
        "/\\evil.example",
        "evil",
        "/a b",
        "/\r\nSet-Cookie:x",
    ] {
        let q = oauth::form(&[("next", bad)]);
        let r = t.call(req(
            "GET",
            &format!("{PREFIX}oauth/oidc/start?{q}"),
            &[],
            None,
        ));
        assert_eq!(refused(&r), "invalid_request", "{bad}");
        let r = t.call(req(
            "POST",
            &format!("{PREFIX}oauth/oidc/start"),
            &[],
            Some(json!({"next": bad})),
        ));
        assert_eq!(r.status, 400, "{bad}");
    }
    assert_eq!(
        super::safe_next("/orgs/x?y=1#z").as_deref(),
        Some("/orgs/x?y=1#z")
    );
    // Unknown providers.
    let r = t.call(req("GET", &format!("{PREFIX}oauth/nope/start"), &[], None));
    assert_eq!(refused(&r), "unknown_provider");
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}oauth/nope/start"),
        &[],
        Some(json!({})),
    ));
    assert_eq!(r.status, 404);
}

#[test]
fn state_and_nonce_must_match() {
    let t = setup(Signer::es256(), false);
    // A state nobody issued.
    let r = t.callback("oidc", "made-up", "isb_oauth=x");
    assert_eq!(refused(&r), "state_invalid");

    // The right state from another browser (no or another binding cookie).
    let f = t.start("oidc", "", &[]);
    let other = format!("isb_oauth={}", "B".repeat(43));
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &other)),
        "state_mismatch"
    );
    // A state is single use, even after a failed attempt.
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &f.cookie)),
        "state_invalid"
    );

    // The state of one provider on another's callback.
    let f = t.start("oidc", "", &[]);
    assert_eq!(
        refused(&t.callback("github", &f.state, &f.cookie)),
        "state_mismatch"
    );

    // An ID token for another sign-in (wrong nonce).
    let f = t.start("oidc", "", &[]);
    t.fake.state.lock().unwrap().nonce = "someone-elses-nonce".into();
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &f.cookie)),
        "provider_error"
    );

    // A PKCE verifier that does not match what the provider saw.
    let f = t.start("oidc", "", &[]);
    t.fake.state.lock().unwrap().challenge = "not-it".into();
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &f.cookie)),
        "provider_error"
    );
    assert_eq!(t.fake.state.lock().unwrap().refused, ["pkce"]);

    // The provider said no.
    let f = t.start("oidc", "", &[]);
    let q = oauth::form(&[("error", "access_denied"), ("state", &f.state)]);
    let r = t.call(req(
        "GET",
        &format!("{PREFIX}oauth/oidc/callback?{q}"),
        &[("Cookie", &f.cookie)],
        None,
    ));
    assert_eq!(refused(&r), "provider_denied");

    // A flow older than 10 minutes.
    let f = t.start("oidc", "", &[]);
    t.clock.fetch_add(601, Ordering::SeqCst);
    assert_eq!(
        refused(&t.callback("oidc", &f.state, &f.cookie)),
        "state_invalid"
    );
}

#[test]
fn github_uses_only_the_primary_verified_email() {
    let t = setup(Signer::es256(), false);
    // The primary is unverified; a verified secondary matching a user does
    // not count.
    t.fake.state.lock().unwrap().gh_emails = vec![
        ("gina@personal.io".into(), true, false),
        ("root@x.io".into(), false, true),
    ];
    let f = t.start("github", "", &[]);
    assert_eq!(
        refused(&t.callback("github", &f.state, &f.cookie)),
        "unverified_email"
    );

    t.fake.state.lock().unwrap().gh_emails = vec![
        ("other@x.io".into(), false, true),
        ("root@x.io".into(), true, true),
    ];
    let f = t.start("github", "next=/x", &[]);
    let r = t.callback("github", &f.state, &f.cookie);
    assert_eq!(
        r.get_header("location"),
        Some("/x"),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let session = set_cookie(&r, "isb_session").unwrap();

    // The identity shows up with its label; it can go while a password
    // remains, but a provider-only user cannot remove their last way in.
    let r = t.call(req(
        "GET",
        &format!("{PREFIX}identities"),
        &[("Cookie", &session)],
        None,
    ));
    let v = json_of(&r);
    let id = &v["identities"][0];
    assert_eq!(
        (id["provider"].as_str(), id["label"].as_str()),
        (Some("github"), Some("GitHub"))
    );
    assert_eq!(id["subject"], "4242");
    let path = format!("{PREFIX}identities/{}", id["id"]);
    assert_eq!(
        t.call(req("DELETE", &path, &[("Cookie", &session)], None))
            .status,
        204
    );
    assert_eq!(
        t.call(req("DELETE", &path, &[("Cookie", &session)], None))
            .status,
        404
    );
}

#[test]
fn provider_only_user_keeps_a_way_in_and_links_more() {
    let t = setup(Signer::es256(), true);
    {
        let mut s = t.fake.state.lock().unwrap();
        s.email = Some("solo@x.io".into());
        s.sub = "solo".into();
    }
    let f = t.start("oidc", "", &[]);
    let session = set_cookie(&t.callback("oidc", &f.state, &f.cookie), "isb_session").unwrap();
    let v = json_of(&t.call(req(
        "GET",
        &format!("{PREFIX}identities"),
        &[("Cookie", &session)],
        None,
    )));
    let path = format!("{PREFIX}identities/{}", v["identities"][0]["id"]);
    let r = t.call(req("DELETE", &path, &[("Cookie", &session)], None));
    assert_eq!(r.status, 409);
    assert!(
        json_of(&r)["message"]
            .as_str()
            .unwrap()
            .contains("last way")
    );

    // Link GitHub while signed in: no new session, back to next.
    t.fake.state.lock().unwrap().gh_emails = vec![("unrelated@x.io".into(), true, false)];
    let f = t.start(
        "github",
        "intent=link&next=/settings",
        &[("Cookie", &session)],
    );
    let cookie = format!("{}; {session}", f.cookie);
    let r = t.callback("github", &f.state, &cookie);
    assert_eq!(r.get_header("location"), Some("/settings"));
    assert!(set_cookie(&r, "isb_session").is_none());
    let v = json_of(&t.call(req(
        "GET",
        &format!("{PREFIX}identities"),
        &[("Cookie", &session)],
        None,
    )));
    assert_eq!(v["identities"].as_array().unwrap().len(), 2);
    // Now the OIDC identity can go.
    assert_eq!(
        t.call(req("DELETE", &path, &[("Cookie", &session)], None))
            .status,
        204
    );

    // Linking needs a session.
    let r = t.call(req(
        "GET",
        &format!("{PREFIX}oauth/github/start?intent=link"),
        &[],
        None,
    ));
    assert_eq!(refused(&r), "forbidden");
}

// ---- passkeys ----

struct PasskeyT {
    t: T,
    session: String,
}

fn passkey_setup() -> PasskeyT {
    let t = setup(Signer::es256(), false);
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}login"),
        &[],
        Some(json!({"email": "root@x.io", "password": PW})),
    ));
    assert_eq!(r.status, 200);
    let session = set_cookie(&r, "isb_session").unwrap();
    PasskeyT { t, session }
}

impl PasskeyT {
    fn register(&self, a: &Authenticator) -> Value {
        let t = &self.t;
        let r = t.call(req(
            "POST",
            &format!("{PREFIX}passkeys/register/options"),
            &[("Cookie", &self.session)],
            None,
        ));
        assert_eq!(r.status, 200);
        let o = json_of(&r)["publicKey"].clone();
        assert_eq!(o["rp"]["id"], "localhost");
        assert_eq!(o["user"]["name"], "root@x.io");
        assert_eq!(o["authenticatorSelection"]["residentKey"], "required");
        let ch = crate::auth::webauthn::unb64(o["challenge"].as_str().unwrap()).unwrap();
        let (cd, att) = a.register("localhost", PUBLIC, &ch);
        let r = t.call(req(
            "POST",
            &format!("{PREFIX}passkeys/register/verify"),
            &[("Cookie", &self.session)],
            Some(json!({"name": "Test key", "credential": {
                "id": b64(&a.credential_id), "rawId": b64(&a.credential_id), "type": "public-key",
                "response": {"clientDataJSON": b64(&cd), "attestationObject": b64(&att),
                             "transports": ["internal", "hybrid"]}}})),
        ));
        assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
        json_of(&r)["passkey"].clone()
    }

    /// Sign in with `a`; returns the response.
    fn login(
        &self,
        a: &mut Authenticator,
        email: Option<&str>,
        user_handle: Option<&[u8]>,
    ) -> Response {
        let t = &self.t;
        let body = email.map(|e| json!({"email": e}));
        let r = t.call(req(
            "POST",
            &format!("{PREFIX}passkeys/login/options"),
            &[],
            body,
        ));
        assert_eq!(r.status, 200);
        let o = json_of(&r)["publicKey"].clone();
        let ch = crate::auth::webauthn::unb64(o["challenge"].as_str().unwrap()).unwrap();
        let (cd, ad, sig) = a.assert("localhost", PUBLIC, &ch);
        t.call(req(
            "POST",
            &format!("{PREFIX}passkeys/login/verify"),
            &[],
            Some(json!({"credential": {
                "id": b64(&a.credential_id), "rawId": b64(&a.credential_id), "type": "public-key",
                "response": {"clientDataJSON": b64(&cd), "authenticatorData": b64(&ad),
                             "signature": b64(&sig), "userHandle": user_handle.map(b64)}}})),
        ))
    }
}

#[test]
fn passkeys_register_and_sign_in() {
    let p = passkey_setup();
    let mut es = Authenticator::es256();
    es.counter = 1;
    let mut ed = Authenticator::ed25519();
    let k = p.register(&es);
    assert_eq!(k["name"], "Test key");
    assert_eq!(k["transports"], json!(["internal", "hybrid"]));
    p.register(&ed);
    let handle = p.t.store.passkey_user_handle(1).unwrap().unwrap();

    // Usernameless (discoverable) sign-in, with the user handle.
    let r = p.login(&mut es, None, Some(&handle));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let s = set_cookie(&r, "isb_session").unwrap();
    assert_eq!(p.t.me(&s)["user"]["email"], "root@x.io");
    // With an email: allowCredentials lists both passkeys.
    let r = p.t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/login/options"),
        &[],
        Some(json!({"email": "root@x.io"})),
    ));
    assert_eq!(
        json_of(&r)["publicKey"]["allowCredentials"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let r = p.login(&mut ed, Some("root@x.io"), None);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    // A passkey offered for another account's email is refused.
    p.t.store
        .create_user("other@x.io", "O", Some(PW), false)
        .unwrap();
    let r = p.login(&mut ed, Some("other@x.io"), None);
    assert_eq!(r.status, 401);
    // A wrong user handle.
    let r = p.login(&mut es, None, Some(b"not the handle"));
    assert_eq!(r.status, 401);

    let list = json_of(&p.t.call(req(
        "GET",
        &format!("{PREFIX}passkeys"),
        &[("Cookie", &p.session)],
        None,
    )));
    assert_eq!(list["passkeys"].as_array().unwrap().len(), 2);
    assert!(list["passkeys"][0]["last_used"].is_number());
}

#[test]
fn passkey_counter_regression_and_replay() {
    let p = passkey_setup();
    let mut a = Authenticator::es256();
    a.counter = 10;
    p.register(&a);
    assert_eq!(p.login(&mut a, None, None).status, 200);
    // A clone whose counter is behind: refused, and no session.
    a.counter = 5;
    let r = p.login(&mut a, None, None);
    assert_eq!(r.status, 401);
    assert_eq!(json_of(&r)["error"], "passkey_rejected");
    assert!(json_of(&r)["message"].as_str().unwrap().contains("counter"));
    assert!(set_cookie(&r, "isb_session").is_none());

    // A challenge is single use: replaying a verified assertion fails.
    a.counter = 20;
    let t = &p.t;
    let o = json_of(&t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/login/options"),
        &[],
        None,
    )));
    let ch = crate::auth::webauthn::unb64(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let (cd, ad, sig) = a.assert("localhost", PUBLIC, &ch);
    let body = json!({"credential": {"rawId": b64(&a.credential_id), "response": {
        "clientDataJSON": b64(&cd), "authenticatorData": b64(&ad), "signature": b64(&sig)}}});
    let verify = format!("{PREFIX}passkeys/login/verify");
    assert_eq!(
        t.call(req("POST", &verify, &[], Some(body.clone()))).status,
        200
    );
    let r = t.call(req("POST", &verify, &[], Some(body)));
    assert_eq!(r.status, 401);
    assert!(
        json_of(&r)["message"]
            .as_str()
            .unwrap()
            .contains("challenge")
    );
}

#[test]
fn passkey_endpoints_need_a_session() {
    let p = passkey_setup();
    let t = &p.t;
    // Not signed in.
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/options"),
        &[],
        None,
    ));
    assert_eq!(r.status, 401);
    // An API token may list but not add or remove.
    let tok = t.store.create_api_token(1, None, "ci", None).unwrap().token;
    let bearer = format!("Bearer {tok}");
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/options"),
        &[("Authorization", &bearer)],
        None,
    ));
    assert_eq!(r.status, 403);
    let r = t.call(req(
        "GET",
        &format!("{PREFIX}passkeys"),
        &[("Authorization", &bearer)],
        None,
    ));
    assert_eq!(r.status, 200);

    // A registration answered for another session's challenge is refused;
    // so is an expired challenge.
    let a = Authenticator::es256();
    let o = json_of(&t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/options"),
        &[("Cookie", &p.session)],
        None,
    )));
    let ch = crate::auth::webauthn::unb64(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let (cd, att) = a.register("localhost", PUBLIC, &ch);
    let other = t.store.create_user("o@x.io", "O", Some(PW), false).unwrap();
    let other_session = format!(
        "isb_session={}",
        t.store
            .start_session(other.id, Default::default())
            .unwrap()
            .token
    );
    let body = json!({"credential": {"response": {"clientDataJSON": b64(&cd), "attestationObject": b64(&att)}}});
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/verify"),
        &[("Cookie", &other_session)],
        Some(body),
    ));
    assert_eq!(r.status, 401);
    assert!(t.store.list_passkeys(other.id).unwrap().is_empty());
    let o = json_of(&t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/options"),
        &[("Cookie", &p.session)],
        None,
    )));
    let ch = crate::auth::webauthn::unb64(o["publicKey"]["challenge"].as_str().unwrap()).unwrap();
    let (cd, att) = a.register("localhost", PUBLIC, &ch);
    t.clock.fetch_add(301, Ordering::SeqCst);
    let body = json!({"credential": {"response": {"clientDataJSON": b64(&cd), "attestationObject": b64(&att)}}});
    let r = t.call(req(
        "POST",
        &format!("{PREFIX}passkeys/register/verify"),
        &[("Cookie", &p.session)],
        Some(body),
    ));
    assert_eq!(r.status, 401, "expired");

    // Removing the only passkey of a password user is fine.
    let k = p.register(&a);
    let path = format!("{PREFIX}passkeys/{}", k["id"]);
    assert_eq!(
        t.call(req("DELETE", &path, &[("Authorization", &bearer)], None))
            .status,
        403
    );
    assert_eq!(
        t.call(req("DELETE", &path, &[("Cookie", &p.session)], None))
            .status,
        204
    );
}
