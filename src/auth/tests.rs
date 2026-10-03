use std::sync::atomic::{AtomicI64, Ordering};

use super::*;

const PW: &str = "correct horse battery";
const DAY: i64 = 86400;

pub(crate) fn fast_config() -> AuthConfig {
    AuthConfig {
        password_cost: PasswordCost::insecure_fast(),
        ..Default::default()
    }
}

/// A store with a clock the test moves.
pub(crate) fn store() -> (AuthStore, Arc<AtomicI64>) {
    store_with(fast_config())
}

pub(crate) fn store_with(cfg: AuthConfig) -> (AuthStore, Arc<AtomicI64>) {
    let t = Arc::new(AtomicI64::new(1_800_000_000));
    let c = t.clone();
    let s = AuthStore::in_memory(cfg)
        .unwrap()
        .with_clock(Arc::new(move || c.load(Ordering::SeqCst)));
    (s, t)
}

fn org(s: &str) -> OrgId {
    OrgId::new(s).unwrap()
}

fn meta(ip: &str) -> LoginMeta {
    LoginMeta {
        user_agent: Some("test/1".into()),
        ip: Some(ip.into()),
    }
}

/// An admin (platform, owner of default) and `ocai` with an owner, an admin
/// and a member.
fn seeded() -> (AuthStore, Arc<AtomicI64>, [User; 4]) {
    let (s, t) = store();
    let root = s.create_first_admin("root@x.io", "Root", PW).unwrap();
    let o = s.create_user("owner@x.io", "O", Some(PW), false).unwrap();
    let a = s.create_user("admin@x.io", "A", Some(PW), false).unwrap();
    let m = s.create_user("member@x.io", "M", Some(PW), false).unwrap();
    s.set_member(&org("ocai"), o.id, Role::Owner).unwrap();
    s.set_member(&org("ocai"), a.id, Role::Admin).unwrap();
    s.set_member(&org("ocai"), m.id, Role::Member).unwrap();
    (s, t, [root, o, a, m])
}

#[test]
fn first_run_setup() {
    let (s, _) = store();
    assert!(s.setup_needed().unwrap());
    assert!(matches!(
        s.create_first_admin("a@x.io", "A", "short"),
        Err(AuthError::Invalid(_))
    ));
    assert!(s.setup_needed().unwrap());
    let u = s.create_first_admin(" Admin@X.io ", " Ada ", PW).unwrap();
    assert_eq!(u.email, "admin@x.io");
    assert_eq!(u.name, "Ada");
    assert!(u.platform_admin && u.has_password && !u.disabled);
    assert!(!s.setup_needed().unwrap());
    assert_eq!(
        s.memberships(u.id).unwrap(),
        vec![Membership {
            org: OrgId::default_org(),
            role: Role::Owner
        }]
    );
    assert!(matches!(
        s.create_first_admin("b@x.io", "B", PW),
        Err(AuthError::Conflict(_))
    ));
    assert!(matches!(
        s.create_user("ADMIN@x.io", "dup", Some(PW), false),
        Err(AuthError::Conflict(_))
    ));
    assert!(s.create_user("not-an-email", "", None, false).is_err());
}

#[test]
fn login_failures_are_indistinguishable() {
    let (s, _) = store();
    s.create_first_admin("a@x.io", "A", PW).unwrap();
    let nopw = s.create_user("nopw@x.io", "", None, false).unwrap();
    assert!(!nopw.has_password);
    let wrong = s
        .login("a@x.io", "wrong password!", meta("1.1.1.1"))
        .unwrap_err();
    let unknown = s.login("b@x.io", PW, meta("1.1.1.2")).unwrap_err();
    let passwordless = s.login("nopw@x.io", PW, meta("1.1.1.3")).unwrap_err();
    for e in [&wrong, &unknown, &passwordless] {
        assert!(matches!(e, AuthError::InvalidCredentials));
    }
    assert_eq!(wrong.to_string(), unknown.to_string());
    assert_eq!(wrong.to_string(), "invalid email or password");
    // Email is case-insensitive and trimmed.
    let n = s.login(" A@X.IO", PW, meta("1.1.1.1")).unwrap();
    assert!(n.token.starts_with("isb_sess_"));
    assert_eq!(n.session.user_agent.as_deref(), Some("test/1"));
    assert_eq!(n.session.ip.as_deref(), Some("1.1.1.1"));
}

#[test]
fn disabled_users_cannot_sign_in_and_lose_sessions() {
    let (s, _, [_, o, ..]) = seeded();
    let n = s.login("owner@x.io", PW, meta("1.1.1.1")).unwrap();
    let t = s
        .create_api_token(o.id, Some(&org("ocai")), "ci", None)
        .unwrap();
    s.set_disabled(o.id, true).unwrap();
    assert!(s.session(&n.token).unwrap().is_none());
    assert!(s.authenticate_token(&t.token).unwrap().is_none());
    assert!(matches!(
        s.login("owner@x.io", PW, meta("1.1.1.1")),
        Err(AuthError::InvalidCredentials)
    ));
    s.set_disabled(o.id, false).unwrap();
    // Re-enabled: the token works again, the session is gone for good.
    assert!(s.authenticate_token(&t.token).unwrap().is_some());
    assert!(s.session(&n.token).unwrap().is_none());
}

#[test]
fn login_is_rate_limited_per_email_and_ip() {
    let (s, t) = store();
    s.create_first_admin("a@x.io", "A", PW).unwrap();
    // Per email: 5 at once, from different IPs.
    for i in 0..5 {
        let r = s.login("a@x.io", "nope nope nope", meta(&format!("10.0.0.{i}")));
        assert!(matches!(r, Err(AuthError::InvalidCredentials)));
    }
    match s.login("a@x.io", PW, meta("10.0.1.1")) {
        Err(AuthError::RateLimited { retry_after }) => assert!(retry_after > 0),
        r => panic!("{r:?}"),
    }
    // A minute later one more attempt is allowed, and the right password works.
    t.fetch_add(60, Ordering::SeqCst);
    assert!(s.login("a@x.io", PW, meta("10.0.1.1")).is_ok());
    // Per IP: 20 at once across emails.
    for i in 0..20 {
        let r = s.login(&format!("u{i}@x.io"), PW, meta("9.9.9.9"));
        assert!(matches!(r, Err(AuthError::InvalidCredentials)), "{i}");
    }
    assert!(matches!(
        s.login("other@x.io", PW, meta("9.9.9.9")),
        Err(AuthError::RateLimited { .. })
    ));
}

#[test]
fn sessions_slide_and_expire() {
    let (s, t) = store();
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let n = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    let (user, sess) = s.session(&n.token).unwrap().unwrap();
    assert_eq!(user.id, u.id);
    assert_eq!(sess.expires_at, sess.created_at + 30 * DAY);
    assert_eq!(sess.idle_expires_at, sess.created_at + 7 * DAY);
    // Used every 6 days, it lives until the 30-day absolute limit.
    for _ in 0..4 {
        t.fetch_add(6 * DAY, Ordering::SeqCst);
        let (_, s2) = s.session(&n.token).unwrap().unwrap();
        assert_eq!(s2.last_seen, t.load(Ordering::SeqCst));
    }
    t.fetch_add(6 * DAY, Ordering::SeqCst); // day 30
    assert!(s.session(&n.token).unwrap().is_none());
    // Deleted, not just refused.
    assert!(s.list_sessions(u.id).unwrap().is_empty());

    // Unused for 7 days, it ends.
    let n = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    t.fetch_add(7 * DAY - 1, Ordering::SeqCst);
    assert!(s.session(&n.token).unwrap().is_some());
    t.fetch_add(7 * DAY, Ordering::SeqCst);
    assert!(s.session(&n.token).unwrap().is_none());
}

#[test]
fn session_lifetimes_are_configurable() {
    let (s, t) = store_with(AuthConfig {
        session_max_age: Duration::from_secs(3600),
        session_idle: Duration::from_secs(600),
        ..fast_config()
    });
    s.create_first_admin("a@x.io", "A", PW).unwrap();
    let n = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    assert_eq!(n.session.idle_expires_at, n.session.created_at + 600);
    t.fetch_add(601, Ordering::SeqCst);
    assert!(s.session(&n.token).unwrap().is_none());
}

#[test]
fn logout_and_revocation() {
    let (s, _) = store();
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let a = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    let b = s.login("a@x.io", PW, meta("1.1.1.2")).unwrap();
    let c = s.login("a@x.io", PW, meta("1.1.1.3")).unwrap();
    assert_eq!(s.list_sessions(u.id).unwrap().len(), 3);
    assert!(s.logout(&a.token).unwrap());
    assert!(!s.logout(&a.token).unwrap());
    assert!(s.session(&a.token).unwrap().is_none());
    assert!(s.revoke_session(u.id, b.session.id).unwrap());
    assert!(s.session(&b.token).unwrap().is_none());
    // Someone else's session id is not yours to end.
    assert!(!s.revoke_session(u.id + 1, c.session.id).unwrap());
    let d = s.login("a@x.io", PW, meta("1.1.1.4")).unwrap();
    assert_eq!(s.revoke_sessions(u.id, Some(d.session.id)).unwrap(), 1);
    assert!(s.session(&c.token).unwrap().is_none());
    assert!(s.session(&d.token).unwrap().is_some());
    // Garbage, and other kinds of token, are not sessions.
    assert!(s.session("isb_sess_nope").unwrap().is_none());
    assert!(s.session("").unwrap().is_none());
    let (inv, _) = secret::new_token(TokenKind::Invitation).unwrap();
    assert!(s.session(&inv).unwrap().is_none());
    // A well-formed token nobody issued.
    let (fake, _) = secret::new_token(TokenKind::Session).unwrap();
    assert!(s.session(&fake).unwrap().is_none());
}

#[test]
fn only_hashes_are_stored() {
    let (s, _) = store();
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let n = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    let t = s.create_api_token(u.id, None, "x", None).unwrap();
    let db = s.db();
    let stored: Vec<u8> = db
        .query_row("SELECT token_hash FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, secret::hash_token(&n.token));
    let stored: Vec<u8> = db
        .query_row("SELECT token_hash FROM api_tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, secret::hash_token(&t.token));
    let pw: String = db
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    assert!(pw.starts_with("$argon2id$") && !pw.contains(PW));
}

#[test]
fn invitations() {
    let (s, t, [root, o, ..]) = seeded();
    let ocai = org("ocai");
    let n = s
        .create_invitation(Some(o.id), &ocai, "New@X.io", Role::Admin)
        .unwrap();
    assert!(n.token.starts_with("isb_inv_"));
    assert_eq!(n.invitation.email, "new@x.io");
    assert_eq!(n.invitation.expires_at - n.invitation.created_at, 7 * DAY);
    assert_eq!(s.list_invitations(&ocai).unwrap().len(), 1);
    assert_eq!(s.invitation(&n.token).unwrap().unwrap().role, Role::Admin);
    // A new address gets an account, signed in by the caller.
    let a = s.accept_invitation(&n.token, "Newbie", PW).unwrap();
    assert!(a.created);
    assert_eq!(a.user.email, "new@x.io");
    assert_eq!(a.membership.role, Role::Admin);
    assert!(s.login("new@x.io", PW, meta("2.2.2.2")).is_ok());
    // Single use.
    assert!(matches!(
        s.accept_invitation(&n.token, "Again", PW),
        Err(AuthError::InvalidToken(_))
    ));
    assert!(s.list_invitations(&ocai).unwrap().is_empty());

    // An existing account must prove its password.
    let n = s
        .create_invitation(Some(o.id), &org("other"), "new@x.io", Role::Member)
        .unwrap();
    assert!(matches!(
        s.accept_invitation(&n.token, "", "not the password"),
        Err(AuthError::InvalidCredentials)
    ));
    let a = s.accept_invitation(&n.token, "", PW).unwrap();
    assert!(!a.created);
    assert_eq!(s.memberships(a.user.id).unwrap().len(), 2);

    // Accepting as a signed-in user needs the invited address.
    let n = s
        .create_invitation(None, &org("third"), "new@x.io", Role::Member)
        .unwrap();
    assert!(matches!(
        s.accept_invitation_as(&n.token, root.id),
        Err(AuthError::Forbidden(_))
    ));
    s.accept_invitation_as(&n.token, a.user.id).unwrap();

    // A lesser invitation never lowers a role.
    let n = s
        .create_invitation(None, &ocai, "new@x.io", Role::Member)
        .unwrap();
    let r = s.accept_invitation_as(&n.token, a.user.id).unwrap();
    assert_eq!(r.membership.role, Role::Admin);

    // Re-inviting replaces the pending invitation.
    let first = s
        .create_invitation(None, &ocai, "late@x.io", Role::Member)
        .unwrap();
    let second = s
        .create_invitation(None, &ocai, "late@x.io", Role::Member)
        .unwrap();
    assert!(s.invitation(&first.token).unwrap().is_none());
    assert!(s.invitation(&second.token).unwrap().is_some());
    // Revoked.
    assert!(s.revoke_invitation(&ocai, second.invitation.id).unwrap());
    assert!(s.invitation(&second.token).unwrap().is_none());
    // Expired.
    let n = s
        .create_invitation(None, &ocai, "slow@x.io", Role::Member)
        .unwrap();
    t.fetch_add(7 * DAY, Ordering::SeqCst);
    assert!(s.invitation(&n.token).unwrap().is_none());
    assert!(matches!(
        s.accept_invitation(&n.token, "Slow", PW),
        Err(AuthError::InvalidToken(_))
    ));
    // A weak password is refused before anything changes.
    let n = s
        .create_invitation(None, &ocai, "weak@x.io", Role::Member)
        .unwrap();
    assert!(matches!(
        s.accept_invitation(&n.token, "W", "short"),
        Err(AuthError::Invalid(_))
    ));
    assert!(s.invitation(&n.token).unwrap().is_some());
}

#[test]
fn api_tokens() {
    let (s, t, [root, o, _, m]) = seeded();
    let ocai = org("ocai");
    // A platform token: the admin's whole reach.
    let p = s.create_api_token(root.id, None, "ops", None).unwrap();
    assert!(p.token.starts_with("isb_tok_"));
    let pr = s.authenticate_token(&p.token).unwrap().unwrap();
    assert!(pr.is_platform_admin());
    assert!(pr.can_admin_org(&ocai));
    assert_eq!(
        pr.kind,
        PrincipalKind::ApiToken {
            id: p.info.id,
            org: None,
            name: "ops".into(),
            scopes: vec![],
        }
    );
    // Non-admins must confine a token to an org they belong to.
    assert!(matches!(
        s.create_api_token(m.id, None, "x", None),
        Err(AuthError::Forbidden(_))
    ));
    assert!(matches!(
        s.create_api_token(m.id, Some(&org("elsewhere")), "x", None),
        Err(AuthError::Forbidden(_))
    ));
    assert!(s.create_api_token(m.id, Some(&ocai), " ", None).is_err());
    let mt = s
        .create_api_token(
            m.id,
            Some(&ocai),
            "deploy",
            Some(Duration::from_secs(90 * 86400)),
        )
        .unwrap();
    assert_eq!(mt.info.expires_at, Some(mt.info.created_at + 90 * DAY));
    let pr = s.authenticate_token(&mt.token).unwrap().unwrap();
    assert_eq!(pr.orgs, vec![(ocai.clone(), Role::Member)]);
    assert!(pr.can_admin_org(&ocai) && !pr.can_manage_members(&ocai));
    assert!(!pr.can_admin_org(&OrgId::default_org()));
    assert_eq!(
        s.api_token(mt.info.id).unwrap().last_used,
        Some(t.load(Ordering::SeqCst))
    );
    // The role is read live: promote, and the token can manage members.
    s.set_member(&ocai, m.id, Role::Admin).unwrap();
    assert!(
        s.authenticate_token(&mt.token)
            .unwrap()
            .unwrap()
            .can_manage_members(&ocai)
    );
    // An admin's org token is confined to the org, not platform-wide.
    let rt = s.create_api_token(root.id, Some(&ocai), "r", None).unwrap();
    let pr = s.authenticate_token(&rt.token).unwrap().unwrap();
    assert!(!pr.is_platform_admin());
    assert_eq!(pr.orgs, vec![(ocai.clone(), Role::Owner)]);
    assert!(!pr.can_admin_org(&OrgId::default_org()));
    // Listing.
    assert_eq!(s.list_api_tokens(m.id).unwrap().len(), 1);
    assert_eq!(s.list_org_api_tokens(&ocai).unwrap().len(), 2);
    assert_eq!(s.list_all_api_tokens().unwrap().len(), 3);
    // Leaving the org takes its tokens along.
    let ot = s.create_api_token(o.id, Some(&ocai), "o", None).unwrap();
    s.set_member(&ocai, m.id, Role::Member).unwrap();
    assert!(s.remove_member(&ocai, m.id).unwrap());
    assert!(s.authenticate_token(&mt.token).unwrap().is_none());
    assert!(s.list_api_tokens(m.id).unwrap().is_empty());
    // Expiry.
    t.fetch_add(90 * DAY, Ordering::SeqCst);
    assert!(s.authenticate_token(&ot.token).unwrap().is_some());
    let short = s
        .create_api_token(o.id, Some(&ocai), "short", Some(Duration::from_secs(60)))
        .unwrap();
    t.fetch_add(60, Ordering::SeqCst);
    assert!(s.authenticate_token(&short.token).unwrap().is_none());
    // Revocation.
    assert!(s.revoke_api_token(ot.info.id).unwrap());
    assert!(!s.revoke_api_token(ot.info.id).unwrap());
    assert!(s.authenticate_token(&ot.token).unwrap().is_none());
    // A platform token stops when its user stops being a platform admin.
    s.set_platform_admin(root.id, false).unwrap();
    assert!(s.authenticate_token(&p.token).unwrap().is_none());
    // Not a token at all, or the wrong kind.
    assert!(s.authenticate_token("isb_tok_x").unwrap().is_none());
    let n = s.login("owner@x.io", PW, meta("1.1.1.1")).unwrap();
    assert!(s.authenticate_token(&n.token).unwrap().is_none());
}

#[test]
fn roles_and_principals() {
    let (s, _, [root, o, a, m]) = seeded();
    let ocai = org("ocai");
    let p = |u: &User| {
        let n = s.start_session(u.id, LoginMeta::default()).unwrap();
        s.authenticate_session(&n.token).unwrap().unwrap()
    };
    let (pr, po, pa, pm) = (p(&root), p(&o), p(&a), p(&m));
    for x in [&po, &pa, &pm] {
        assert!(x.can_admin_org(&ocai));
        assert!(!x.can_admin_org(&OrgId::default_org()));
        assert!(!x.is_platform_admin());
    }
    assert!(po.can_manage_members(&ocai) && po.can_delete_org(&ocai));
    assert!(pa.can_manage_members(&ocai) && !pa.can_delete_org(&ocai));
    assert!(!pm.can_manage_members(&ocai) && !pm.can_delete_org(&ocai));
    assert!(pr.is_platform_admin() && pr.can_delete_org(&ocai) && pr.can_admin_org(&org("any")));
    assert_eq!(po.max_grant(&ocai), Some(Role::Owner));
    assert_eq!(pa.max_grant(&ocai), Some(Role::Admin));
    assert_eq!(pm.max_grant(&ocai), None);
    assert_eq!(pr.max_grant(&org("any")), Some(Role::Owner));
    assert!(pm.session_id().is_some());
    // Roles are data: parse, print, order.
    for r in Role::ALL {
        assert_eq!(Role::parse(r.as_str()).unwrap(), r);
    }
    assert!(Role::parse("god").is_err());
    assert!(Role::Viewer < Role::Member && Role::Member < Role::Admin && Role::Admin < Role::Owner);
    assert!(Role::Member.can(Permission::AdminOrg));
    assert!(Role::Viewer.can(Permission::ReadOrg) && !Role::Viewer.can(Permission::AdminOrg));
}

#[test]
fn viewers_and_scoped_tokens() {
    let (s, _) = store();
    let root = s.create_first_admin("root@x.io", "", PW).unwrap();
    let v = s.create_user("v@x.io", "", Some(PW), false).unwrap();
    let ocai = org("ocai");
    s.set_member(&ocai, v.id, Role::Viewer).unwrap();
    let t = s.create_api_token(v.id, Some(&ocai), "ro", None).unwrap();
    let pr = s.authenticate_token(&t.token).unwrap().unwrap();
    assert!(pr.can_read_org(&ocai) && !pr.can_admin_org(&ocai));
    assert!(!pr.restricted());
    // Scopes are checked, normalized and kept with the token.
    let bad = s.create_api_token_scoped(root.id, None, "x", None, &["root".into()]);
    assert!(matches!(bad, Err(AuthError::Invalid(_))));
    let t = s
        .create_api_token_scoped(
            root.id,
            Some(&ocai),
            "ci",
            None,
            &["deploy".into(), "tool:app_*".into(), "deploy".into()],
        )
        .unwrap();
    assert_eq!(t.info.scopes, ["deploy", "tool:app_*"]);
    let pr = s.authenticate_token(&t.token).unwrap().unwrap();
    assert_eq!(pr.scopes(), ["deploy", "tool:app_*"]);
    assert!(pr.restricted());
    assert_eq!(
        s.api_token(t.info.id).unwrap().scopes,
        ["deploy", "tool:app_*"]
    );
    let t = s
        .create_api_token_scoped(root.id, Some(&ocai), "all", None, &["admin".into()])
        .unwrap();
    assert!(
        !s.authenticate_token(&t.token)
            .unwrap()
            .unwrap()
            .restricted()
    );
}

#[test]
fn org_keeps_an_owner() {
    let (s, _, [_, o, a, _]) = seeded();
    let ocai = org("ocai");
    assert!(matches!(
        s.set_member(&ocai, o.id, Role::Admin),
        Err(AuthError::Conflict(_))
    ));
    assert!(matches!(
        s.remove_member(&ocai, o.id),
        Err(AuthError::Conflict(_))
    ));
    s.set_member(&ocai, a.id, Role::Owner).unwrap();
    s.set_member(&ocai, o.id, Role::Admin).unwrap();
    assert!(s.remove_member(&ocai, o.id).unwrap());
    assert_eq!(s.list_members(&ocai).unwrap().len(), 2);
    assert!(matches!(
        s.set_member(&ocai, 9999, Role::Member),
        Err(AuthError::NotFound(_))
    ));
    // Deleting an org takes everything in it along.
    s.create_invitation(None, &ocai, "z@x.io", Role::Member)
        .unwrap();
    assert!(s.delete_org(&ocai).unwrap());
    assert!(s.list_members(&ocai).unwrap().is_empty());
    assert!(s.list_invitations(&ocai).unwrap().is_empty());
    assert!(!s.list_orgs().unwrap().contains(&ocai));
}

#[test]
fn change_password() {
    let (s, _) = store();
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let keep = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    let other = s.login("a@x.io", PW, meta("1.1.1.2")).unwrap();
    assert!(matches!(
        s.change_password(u.id, "wrong wrong wrong", "new password 123", None),
        Err(AuthError::Forbidden(_))
    ));
    assert!(matches!(
        s.change_password(u.id, PW, "short", None),
        Err(AuthError::Invalid(_))
    ));
    s.change_password(u.id, PW, "new password 123", Some(keep.session.id))
        .unwrap();
    assert!(s.session(&keep.token).unwrap().is_some());
    assert!(s.session(&other.token).unwrap().is_none());
    assert!(s.login("a@x.io", PW, meta("1.1.1.1")).is_err());
    assert!(
        s.login("a@x.io", "new password 123", meta("1.1.1.1"))
            .is_ok()
    );
    // set_password (CLI) ends every session.
    s.set_password(u.id, "third password!").unwrap();
    assert!(s.session(&keep.token).unwrap().is_none());
}

#[test]
fn password_reset() {
    let (s, t) = store();
    let u = s.create_first_admin("a@x.io", "A", PW).unwrap();
    let sess = s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    assert!(s.request_password_reset("nobody@x.io").unwrap().is_none());
    assert!(s.request_password_reset("garbage").unwrap().is_none());
    let old = s.request_password_reset("A@x.io").unwrap().unwrap();
    let tok = s.request_password_reset("a@x.io").unwrap().unwrap();
    assert!(tok.starts_with("isb_rst_"));
    // Only the newest link works.
    assert!(matches!(
        s.reset_password(&old, "brand new password"),
        Err(AuthError::InvalidToken(_))
    ));
    assert!(matches!(
        s.reset_password(&tok, "short"),
        Err(AuthError::Invalid(_))
    ));
    let r = s.reset_password(&tok, "brand new password").unwrap();
    assert_eq!(r.id, u.id);
    assert!(s.session(&sess.token).unwrap().is_none());
    assert!(
        s.login("a@x.io", "brand new password", meta("1.1.1.1"))
            .is_ok()
    );
    // Single use.
    assert!(s.reset_password(&tok, "another password!").is_err());
    // Expires after an hour.
    let tok = s.request_password_reset("a@x.io").unwrap().unwrap();
    t.fetch_add(3600, Ordering::SeqCst);
    assert!(matches!(
        s.reset_password(&tok, "another password!"),
        Err(AuthError::InvalidToken(_))
    ));
    // Rate limited per email: 3 at once, then one per 15 minutes.
    for _ in 0..3 {
        assert!(s.request_password_reset("a@x.io").is_ok());
    }
    assert!(matches!(
        s.request_password_reset("a@x.io"),
        Err(AuthError::RateLimited { .. })
    ));
}

#[test]
fn prune_drops_expired_rows() {
    let (s, t) = store();
    s.create_first_admin("a@x.io", "A", PW).unwrap();
    s.login("a@x.io", PW, meta("1.1.1.1")).unwrap();
    s.create_invitation(None, &org("x"), "b@x.io", Role::Member)
        .unwrap();
    t.fetch_add(31 * DAY, Ordering::SeqCst);
    s.prune().unwrap();
    let db = s.db();
    let n: i64 = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions) + (SELECT COUNT(*) FROM invitations)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn file_store_is_shared_between_handles() {
    // The daemon and the CLI open the same file at once.
    let dir = tempfile::tempdir().unwrap();
    let p = db_path(dir.path());
    let a = AuthStore::open_with(&p, fast_config()).unwrap();
    let b = AuthStore::open_with(&p, fast_config()).unwrap();
    a.create_first_admin("a@x.io", "A", PW).unwrap();
    assert!(!b.setup_needed().unwrap());
    let u = b.user_by_email("a@x.io").unwrap().unwrap();
    let t = b.create_api_token(u.id, None, "cli", None).unwrap();
    assert!(a.authenticate_token(&t.token).unwrap().is_some());
    assert!(b.revoke_api_token(t.info.id).unwrap());
    assert!(a.authenticate_token(&t.token).unwrap().is_none());
}

#[test]
fn emails() {
    assert_eq!(normalize_email(" A@B.c ").unwrap(), "a@b.c");
    for bad in ["", "a", "@b", "a@", "a@@b", "a b@c", "a@.b"] {
        assert!(normalize_email(bad).is_err(), "{bad}");
    }
}
