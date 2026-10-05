use super::*;
use crate::auth::AuthConfig;
use crate::server::access::tests as at;
use crate::server::tailnet::{AllowList, Whois, WhoisFetcher};
use serde_json::json;

fn req(peer: &str, headers: &[(&str, &str)]) -> Request {
    Request {
        method: "GET".into(),
        path: "/mcp".into(),
        query: None,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: Vec::new(),
        peer: Peer::Tcp(peer.parse().unwrap()),
    }
}

fn store() -> Arc<AuthStore> {
    let c = AuthConfig {
        password_cost: crate::auth::secret::PasswordCost::insecure_fast(),
        ..Default::default()
    };
    Arc::new(AuthStore::in_memory(c).unwrap())
}

fn label(r: Resolved) -> Option<String> {
    match r {
        Resolved::Superadmin(s) => Some(s.label()),
        _ => None,
    }
}

#[test]
fn access_allow_lists() {
    assert!(AccessAllowList::parse("").is_err());
    assert!(AccessAllowList::parse("*@example.com").is_err());
    assert!(AccessAllowList::parse("@example.com").is_err());
    let a = AccessAllowList::parse("Alice@Example.com, abc123.access").unwrap();
    let id = |email: Option<&str>, cn: Option<&str>| Identity {
        email: email.map(String::from),
        sub: "s".into(),
        common_name: cn.map(String::from),
    };
    assert!(a.admits(&id(Some("alice@example.com"), None)));
    assert!(a.admits(&id(Some("ALICE@example.com"), None)));
    assert!(!a.admits(&id(Some("bob@example.com"), None)));
    assert!(!a.admits(&id(Some("alice@example.com.evil"), None)));
    assert!(a.admits(&id(None, Some("abc123.access"))));
    assert!(!a.admits(&id(None, Some("other.access"))));
    // A service token's id never matches as an email, nor the reverse.
    let b = AccessAllowList::parse("alice@example.com").unwrap();
    assert!(!b.admits(&id(None, Some("alice@example.com"))));
}

#[test]
fn tokens_decide_alone() {
    let s = store();
    let t = s.create_superadmin_token("agent", None).unwrap();
    let g = Gate::new(s.clone(), None, None);
    let auth = format!("Bearer {}", t.token);
    assert_eq!(
        label(g.resolve(&req("127.0.0.1:1", &[("Authorization", &auth)]), None)).as_deref(),
        Some("token:agent")
    );
    let bad = format!("Bearer {}x", t.token);
    assert!(matches!(
        g.resolve(&req("127.0.0.1:1", &[("Authorization", &bad)]), None),
        Resolved::Refused
    ));
    // An ordinary API token is not the gate's.
    assert!(matches!(
        g.resolve(
            &req("127.0.0.1:1", &[("Authorization", "Bearer isb_tok_x")]),
            None
        ),
        Resolved::None
    ));
}

/// `ISB_DEV_SUPERADMIN`: a loopback request with no credential, as the isb
/// user when there is one; anything with a credential, or not on loopback,
/// is judged as before. Off, the same request is nobody.
#[cfg(debug_assertions)]
#[test]
fn dev_superadmin_signs_in_bare_loopback_requests() {
    let s = store();
    let bare = req("127.0.0.1:1", &[]);
    let off = Gate::new(s.clone(), None, None);
    assert!(matches!(off.resolve(&bare, None), Resolved::None));
    let g = Gate::new(s.clone(), None, None).with_dev(Some("dev@dev.com".into()));
    match g.resolve(&bare, None) {
        Resolved::Superadmin(sa) => {
            assert_eq!(sa.label(), "dev:dev@dev.com");
            assert!(!sa.has_account());
            assert!(sa.source.is_ambient());
        }
        _ => panic!("no dev superadmin"),
    }
    let dev = s.create_user("dev@dev.com", "Dev", None, false).unwrap();
    match g.resolve(&bare, None) {
        Resolved::Superadmin(sa) => assert_eq!(sa.principal.user.id, dev.id),
        _ => panic!("no dev superadmin"),
    }
    for creds in [
        &[("Cookie", "isb_session=isb_sess_x")][..],
        &[("Authorization", "Bearer isb_tok_x")],
        &[(ASSERTION_HEADER, "x")],
    ] {
        assert!(matches!(
            g.resolve(&req("127.0.0.1:1", creds), None),
            Resolved::None
        ));
    }
    assert!(matches!(
        g.resolve(&req("10.0.0.5:1", &[]), None),
        Resolved::None
    ));
}

#[test]
fn access_needs_a_verified_listed_identity_and_this_host() {
    let s = store();
    let alice = s
        .create_user("alice@example.com", "Alice", None, false)
        .unwrap();
    let (v, _) = at::validator();
    let g = Gate::new(
        s.clone(),
        None,
        Some((
            Arc::new(v),
            AccessAllowList::parse("alice@example.com,svc.access").unwrap(),
            vec!["https://isb.example.com".replace("https://", "")],
        )),
    );
    let token = at::sign(&at::header(), &at::claims());
    let host = ("Host", "isb.example.com");
    let ok = req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &token), host]);
    match g.resolve(&ok, None) {
        Resolved::Superadmin(sa) => {
            assert_eq!(sa.label(), "access:alice@example.com");
            // Acts as the isb user with that email.
            assert_eq!(sa.principal.user.id, alice.id);
        }
        _ => panic!("expected a superadmin"),
    }
    // A forged (unsigned) assertion.
    let forged = req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", "a.b.c"), host]);
    assert!(label(g.resolve(&forged, None)).is_none());
    // Another email.
    let mut c = at::claims();
    c["email"] = json!("bob@example.com");
    let bob = at::sign(&at::header(), &c);
    assert!(
        label(g.resolve(
            &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &bob), host]),
            None
        ))
        .is_none()
    );
    // DNS rebinding: another Host.
    let evil = req(
        "127.0.0.1:1",
        &[
            ("Cf-Access-Jwt-Assertion", &token),
            ("Host", "evil.example"),
        ],
    );
    assert!(label(g.resolve(&evil, None)).is_none());
    // Off the loopback listeners Access guards.
    let tail = req("100.64.0.1:1", &[("Cf-Access-Jwt-Assertion", &token), host]);
    assert!(label(g.resolve(&tail, None)).is_none());
    // A service token by client id, synthetic.
    let mut c = at::claims();
    c.as_object_mut().unwrap().remove("email");
    c["common_name"] = json!("svc.access");
    let svc = at::sign(&at::header(), &c);
    match g.resolve(
        &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &svc), host]),
        None,
    ) {
        Resolved::Superadmin(sa) => {
            assert_eq!(sa.label(), "access:svc.access");
            assert!(!sa.has_account());
        }
        _ => panic!("expected a superadmin"),
    }
}

#[test]
fn tailnet_acts_as_the_user_or_synthetic() {
    let s = store();
    let me = s.create_user("me@example.com", "Me", None, false).unwrap();
    let fetch: WhoisFetcher = Arc::new(|p: std::net::SocketAddr| {
        Ok(match p.ip().to_string().as_str() {
            "100.64.0.1" => Whois {
                login: "me@example.com".into(),
                node: "laptop.t.ts.net".into(),
                tags: vec![],
            },
            _ => Whois {
                login: "tagged-devices".into(),
                node: "agent.t.ts.net".into(),
                tags: vec!["tag:agents".into()],
            },
        })
    });
    let t = Tailnet::new(
        AllowList::parse("me@example.com,tag:agents").unwrap(),
        fetch,
        vec!["100.86.22.100".into()],
    );
    let g = Gate::new(s, Some(t), None);
    let host = ("Host", "100.86.22.100:18995");
    match g.resolve(&req("100.64.0.1:1", &[host]), None) {
        Resolved::Superadmin(sa) => {
            assert_eq!(sa.label(), "tailnet:me@example.com");
            assert_eq!(sa.principal.user.id, me.id);
        }
        _ => panic!(),
    }
    match g.resolve(&req("100.64.0.2:1", &[host]), None) {
        Resolved::Superadmin(sa) => {
            assert_eq!(sa.label(), "tailnet:agent.t.ts.net");
            assert!(!sa.has_account());
        }
        _ => panic!(),
    }
    // A bearer token takes the request away from the tailnet.
    assert!(matches!(
        g.resolve(
            &req(
                "100.64.0.1:1",
                &[host, ("Authorization", "Bearer isb_tok_x")]
            ),
            None
        ),
        Resolved::None
    ));
}

fn agent_store() -> (Arc<AuthStore>, crate::org::OrgId, crate::org::OrgId) {
    let s = store();
    let acme = crate::org::OrgId::new("acme").unwrap();
    let beta = crate::org::OrgId::new("beta").unwrap();
    s.ensure_org(&acme).unwrap();
    s.ensure_org(&beta).unwrap();
    (s, acme, beta)
}

#[test]
fn tailnet_agents_are_pinned_to_the_orgs_that_map_them() {
    use crate::auth::Role;
    use crate::auth::agent_identities::AgentKind;
    let (s, acme, beta) = agent_store();
    let map = |o: &crate::org::OrgId, subj: &str, r| {
        s.set_agent_identity(o, AgentKind::Tailnet, subj, r, "", "t")
            .unwrap()
    };
    map(&acme, "tag:agents", Role::Member);
    map(&beta, "me@example.com", Role::Viewer);
    let fetch: WhoisFetcher = Arc::new(|p: std::net::SocketAddr| {
        Ok(match p.ip().to_string().as_str() {
            "100.64.0.1" => Whois {
                login: "me@example.com".into(),
                node: "laptop.t.ts.net".into(),
                tags: vec![],
            },
            "100.64.0.2" => Whois {
                login: "tagged-devices".into(),
                node: "bot.t.ts.net".into(),
                tags: vec!["tag:agents".into()],
            },
            // A tagged node owned by the mapped login.
            "100.64.0.3" => Whois {
                login: "me@example.com".into(),
                node: "owned.t.ts.net".into(),
                tags: vec!["tag:other".into()],
            },
            _ => Whois {
                login: "stranger@example.com".into(),
                node: "x.t.ts.net".into(),
                tags: vec![],
            },
        })
    });
    // No superadmin allow list at all: agents still resolve.
    let t = Tailnet::new(AllowList::default(), fetch, vec!["100.86.22.100".into()]);
    let g = Gate::new(s, Some(t), None).with_agents(vec!["100.86.22.100:18995".into()], None);
    assert_eq!(g.agent_ways().tailnet_listen, vec!["100.86.22.100:18995"]);
    let host = ("Host", "100.86.22.100:18995");
    let who = |peer: &str, h: &[(&str, &str)]| g.agent(&req(peer, h), None);
    // An untagged node by login: viewer in beta only.
    let p = who("100.64.0.1:1", &[host]).unwrap();
    assert_eq!(p.user.email, "tailnet:me@example.com");
    assert_eq!(p.orgs, vec![(beta.clone(), Role::Viewer)]);
    assert!(p.role_in(&acme).is_none() && !p.platform_admin && p.user.id == 0);
    // A tagged node by its tag: member in acme only.
    let p = who("100.64.0.2:1", &[host]).unwrap();
    assert_eq!(p.user.email, "tailnet:bot.t.ts.net");
    assert_eq!(p.orgs, vec![(acme, Role::Member)]);
    // A tagged node is never its owner's login.
    assert!(who("100.64.0.3:1", &[host]).is_none());
    assert!(who("100.64.0.9:1", &[host]).is_none());
    // Not a tailnet address, or another Host (DNS rebinding).
    assert!(who("192.168.1.5:1", &[host]).is_none());
    assert!(who("100.64.0.1:1", &[("Host", "evil.example")]).is_none());
    assert!(who("100.64.0.1:1", &[]).is_none());
    // A bearer token takes the request away from the tailnet.
    assert!(who("100.64.0.1:1", &[host, ("Authorization", "Bearer x")]).is_none());
    // And a tailnet agent is no superadmin.
    assert!(matches!(
        g.resolve(&req("100.64.0.1:1", &[host]), None),
        Resolved::None
    ));
    assert!(
        g.tailnet()
            .unwrap()
            .superadmin(&req("100.64.0.1:1", &[host]))
            .is_none()
    );
}

#[test]
fn access_agents_need_a_verified_mapped_identity_that_is_not_a_user() {
    use crate::auth::Role;
    use crate::auth::agent_identities::AgentKind;
    let (s, acme, _beta) = agent_store();
    let map = |subj: &str| {
        s.set_agent_identity(&acme, AgentKind::Access, subj, Role::Admin, "", "t")
            .unwrap()
    };
    map("svc.access");
    map("alice@example.com");
    // The isb user's email acts as that user (the authn hook's job); the
    // gate never makes it an agent.
    s.create_user("alice@example.com", "Alice", None, false)
        .ok();
    let (v, _) = at::validator();
    let g = Gate::new(s, None, None).with_agents(
        Vec::new(),
        Some((Arc::new(v), vec!["isb.example.com".into()])),
    );
    assert!(g.agent_ways().access);
    let host = ("Host", "isb.example.com");
    let token = at::sign(&at::header(), &at::claims());
    // The test claims' email is alice's: an isb user, so no agent.
    assert!(
        g.agent(
            &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &token), host]),
            None
        )
        .is_none()
    );
    let mut c = at::claims();
    c.as_object_mut().unwrap().remove("email");
    c["common_name"] = json!("svc.access");
    let svc = at::sign(&at::header(), &c);
    let ok = req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &svc), host]);
    let p = g.agent(&ok, None).unwrap();
    assert_eq!(p.user.email, "access:svc.access");
    assert_eq!(p.orgs, vec![(acme.clone(), Role::Admin)]);
    // An unmapped client id, a forged assertion, a foreign Host, a
    // non-loopback peer.
    c["common_name"] = json!("other.access");
    let other = at::sign(&at::header(), &c);
    assert!(
        g.agent(
            &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &other), host]),
            None
        )
        .is_none()
    );
    assert!(
        g.agent(
            &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", "a.b.c"), host]),
            None
        )
        .is_none()
    );
    assert!(
        g.agent(
            &req(
                "127.0.0.1:1",
                &[("Cf-Access-Jwt-Assertion", &svc), ("Host", "evil.example")]
            ),
            None
        )
        .is_none()
    );
    assert!(
        g.agent(
            &req("100.64.0.1:1", &[("Cf-Access-Jwt-Assertion", &svc), host]),
            None
        )
        .is_none()
    );
    // Never a superadmin.
    assert!(matches!(g.resolve(&ok, None), Resolved::None));
}

#[test]
fn edge_identities_are_people_the_front_door_verified() {
    use crate::auth::agent_identities::AgentKind;
    let fetch: WhoisFetcher = Arc::new(|p: std::net::SocketAddr| {
        Ok(match p.ip().to_string().as_str() {
            "100.64.0.1" => Whois {
                login: "me@example.com".into(),
                node: "laptop.t.ts.net".into(),
                tags: vec![],
            },
            "100.64.0.2" => Whois {
                login: "tagged-devices".into(),
                node: "bot.t.ts.net".into(),
                tags: vec!["tag:agents".into()],
            },
            _ => Whois {
                login: "you@github".into(),
                node: "x.t.ts.net".into(),
                tags: vec![],
            },
        })
    });
    let host = ("Host", "100.86.22.100:18995");
    let hosts = vec!["100.86.22.100".into()];
    // No superadmin list: any person on the tailnet may claim setup.
    let t = Tailnet::new(AllowList::default(), fetch.clone(), hosts.clone());
    let g = Gate::new(store(), Some(t), None);
    let e = g.edge(&req("100.64.0.1:1", &[host]), None).unwrap();
    assert_eq!(e.kind, AgentKind::Tailnet);
    assert_eq!(e.email.as_deref(), Some("me@example.com"));
    assert_eq!(e.node.as_deref(), Some("laptop.t.ts.net"));
    assert!(e.can_claim);
    // A login that is no address carries no email.
    let e = g.edge(&req("100.64.0.9:1", &[host]), None).unwrap();
    assert_eq!((e.name.as_str(), e.email), ("you@github", None));
    // A tagged node is a machine, a bearer token decides alone, and the
    // Host must be this server's.
    assert!(g.edge(&req("100.64.0.2:1", &[host]), None).is_none());
    assert!(
        g.edge(
            &req("100.64.0.1:1", &[host, ("Authorization", "Bearer x")]),
            None
        )
        .is_none()
    );
    assert!(
        g.edge(&req("100.64.0.1:1", &[("Host", "evil.example")]), None)
            .is_none()
    );
    // With a list, only who is on it may claim; everyone else is still
    // an edge identity, to sign in as their own account.
    let t = Tailnet::new(AllowList::parse("ops@example.com").unwrap(), fetch, hosts);
    let g = Gate::new(store(), Some(t), None);
    assert!(
        !g.edge(&req("100.64.0.1:1", &[host]), None)
            .unwrap()
            .can_claim
    );
}

#[test]
fn access_edge_identities_need_a_verified_user_assertion() {
    use crate::auth::agent_identities::AgentKind;
    let (v, _) = at::validator();
    let v = Arc::new(v);
    let host = ("Host", "isb.example.com");
    let hosts = vec!["isb.example.com".to_string()];
    let g =
        Gate::new(store(), None, None).with_agents(Vec::new(), Some((v.clone(), hosts.clone())));
    let token = at::sign(&at::header(), &at::claims());
    let ok = req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &token), host]);
    let e = g.edge(&ok, None).unwrap();
    assert_eq!(e.kind, AgentKind::Access);
    assert_eq!(e.subject, "user-1");
    assert_eq!(e.email.as_deref(), Some("alice@example.com"));
    assert!(e.can_claim);
    // Forged, from another Host, not through the tunnel, or a service
    // token: none is a person Access verified here.
    let forged = req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", "a.b.c"), host]);
    assert!(g.edge(&forged, None).is_none());
    let foreign = req(
        "127.0.0.1:1",
        &[
            ("Cf-Access-Jwt-Assertion", &token),
            ("Host", "evil.example"),
        ],
    );
    assert!(g.edge(&foreign, None).is_none());
    assert!(g.edge(&req("127.0.0.1:1", &[host]), None).is_none());
    let mut c = at::claims();
    c.as_object_mut().unwrap().remove("email");
    c["common_name"] = json!("svc.access");
    let svc = at::sign(&at::header(), &c);
    assert!(
        g.edge(
            &req("127.0.0.1:1", &[("Cf-Access-Jwt-Assertion", &svc), host]),
            None
        )
        .is_none()
    );
    // Off the --superadmin-access list: verified, but no claim.
    let list = AccessAllowList::parse("bob@example.com").unwrap();
    let g = Gate::new(store(), None, Some((v.clone(), list, hosts.clone())))
        .with_agents(Vec::new(), Some((v, hosts)));
    assert!(!g.edge(&ok, None).unwrap().can_claim);
}
