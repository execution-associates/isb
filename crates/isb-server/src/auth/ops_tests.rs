//! The account rules every surface shares: roles, scopes, workspaces and
//! who may mint a token.

use super::*;
use crate::auth::tests::store;
use crate::auth::{AuthStore, User};

const PW: &str = "correct horse battery";

fn org(s: &str) -> OrgId {
    OrgId::new(s).unwrap()
}

/// `u` signed in as `kind`, with their memberships.
fn as_kind(s: &AuthStore, u: &User, kind: PrincipalKind) -> Principal {
    let orgs: Vec<(OrgId, Role)> = s
        .memberships(u.id)
        .unwrap()
        .into_iter()
        .map(|m| (m.org, m.role))
        .collect();
    // An org token is confined to its org, as authenticate_token does.
    let orgs = match &kind {
        PrincipalKind::ApiToken { org: Some(o), .. } => {
            orgs.into_iter().filter(|(x, _)| x == o).collect()
        }
        _ => orgs,
    };
    let confined = matches!(&kind, PrincipalKind::ApiToken { org: Some(_), .. });
    Principal {
        user: u.clone(),
        kind,
        orgs,
        platform_admin: u.platform_admin && !confined,
    }
}

fn session(s: &AuthStore, u: &User) -> Principal {
    as_kind(s, u, PrincipalKind::Session { id: 1 })
}

fn org_token(s: &AuthStore, u: &User, o: &str, scopes: &[&str]) -> Principal {
    as_kind(
        s,
        u,
        PrincipalKind::ApiToken {
            id: 9,
            org: Some(org(o)),
            name: "t".into(),
            scopes: scopes.iter().map(|x| x.to_string()).collect(),
        },
    )
}

struct World {
    s: AuthStore,
    root: User,
    owner: User,
    admin: User,
    member: User,
    viewer: User,
    outsider: User,
}

fn world() -> World {
    let (s, _) = store();
    let root = s.create_user("root@x.io", "Root", Some(PW), true).unwrap();
    let mk = |e: &str, r: Option<Role>| {
        let u = s.create_user(e, e, Some(PW), false).unwrap();
        if let Some(r) = r {
            s.ensure_org(&org("acme")).unwrap();
            s.set_member(&org("acme"), u.id, r).unwrap();
        }
        u
    };
    let owner = mk("owner@x.io", Some(Role::Owner));
    let admin = mk("admin@x.io", Some(Role::Admin));
    let member = mk("member@x.io", Some(Role::Member));
    let viewer = mk("viewer@x.io", Some(Role::Viewer));
    let outsider = mk("out@x.io", None);
    World {
        s,
        root,
        owner,
        admin,
        member,
        viewer,
        outsider,
    }
}

fn forbidden<T: std::fmt::Debug>(r: R<T>) -> bool {
    matches!(r, Err(AuthError::Forbidden(_)))
}

fn hidden<T: std::fmt::Debug>(r: R<T>) -> bool {
    matches!(r, Err(AuthError::NotFound(_)))
}

#[test]
fn members_follow_the_role_matrix() {
    let w = world();
    let (s, acme) = (&w.s, org("acme"));
    // Every member reads the member list; outsiders do not learn the org.
    for u in [&w.owner, &w.admin, &w.member, &w.viewer, &w.root] {
        let v = members(s, &session(s, u), &acme).unwrap();
        assert_eq!(v["members"].as_array().unwrap().len(), 4);
    }
    assert!(hidden(members(s, &session(s, &w.outsider), &acme)));
    assert!(hidden(invitations(s, &session(s, &w.outsider), &acme)));
    // Invitations and org tokens: owners and admins only.
    assert!(invitations(s, &session(s, &w.admin), &acme).is_ok());
    assert!(org_tokens(s, &session(s, &w.owner), &acme).is_ok());
    for u in [&w.member, &w.viewer] {
        let p = session(s, u);
        assert!(forbidden(invitations(s, &p, &acme)));
        assert!(forbidden(org_tokens(s, &p, &acme)));
        assert!(forbidden(invite(s, &p, &acme, "n@x.io", None, None)));
        assert!(forbidden(set_role(s, &p, &acme, w.viewer.id, Role::Member)));
    }
    // An admin manages members and admins, never owners.
    let admin = session(s, &w.admin);
    assert!(forbidden(invite(
        s,
        &admin,
        &acme,
        "n@x.io",
        Some(Role::Owner),
        None
    )));
    assert!(forbidden(set_role(
        s,
        &admin,
        &acme,
        w.owner.id,
        Role::Member
    )));
    assert!(forbidden(set_role(
        s,
        &admin,
        &acme,
        w.member.id,
        Role::Owner
    )));
    assert!(forbidden(remove_member(s, &admin, &acme, w.owner.id)));
    set_role(s, &admin, &acme, w.viewer.id, Role::Member).unwrap();
    // The invitation's token and link come back once.
    let v = invite(
        s,
        &admin,
        &acme,
        "n@x.io",
        Some(Role::Admin),
        Some("https://isb.x.io/"),
    )
    .unwrap();
    let token = v["token"].as_str().unwrap();
    assert_eq!(v["link"], format!("https://isb.x.io/invite#{token}"));
    let id = v["invitation"]["id"].as_i64().unwrap();
    revoke_invitation(s, &admin, &acme, id).unwrap();
    assert!(hidden(revoke_invitation(s, &admin, &acme, id)));
    // An owner makes owners; anyone may leave.
    set_role(s, &session(s, &w.owner), &acme, w.admin.id, Role::Owner).unwrap();
    remove_member(s, &session(s, &w.viewer), &acme, w.viewer.id).unwrap();
    assert!(forbidden(remove_member(
        s,
        &session(s, &w.member),
        &acme,
        w.admin.id
    )));
}

#[test]
fn scoped_tokens_only_read_and_workspaces_reach_nothing() {
    let w = world();
    let (s, acme) = (&w.s, org("acme"));
    let read = org_token(s, &w.owner, "acme", &["read"]);
    assert!(members(s, &read, &acme).is_ok());
    assert!(tokens(s, &read, None).is_ok());
    assert!(forbidden(invite(s, &read, &acme, "n@x.io", None, None)));
    assert!(forbidden(add_ssh_key(s, &read, "ssh-ed25519 AAAA", None)));
    assert!(forbidden(revoke_token(s, &read, 1)));
    assert!(forbidden(revoke_session(s, &read, 1)));
    // `admin` in the scopes lifts it.
    let admin = org_token(s, &w.owner, "acme", &["admin"]);
    assert!(invite(s, &admin, &acme, "n@x.io", None, None).is_ok());
    // A workspace is nobody's account.
    let ws = Principal::workspace(&acme, "dev", Role::Admin);
    assert!(forbidden(members(s, &ws, &acme)));
    assert!(forbidden(invitations(s, &ws, &acme)));
    assert!(forbidden(invite(s, &ws, &acme, "n@x.io", None, None)));
    assert!(forbidden(tokens(s, &ws, None)));
    assert!(forbidden(org_tokens(s, &ws, &acme)));
    assert!(forbidden(ssh_keys(s, &ws)));
    assert!(forbidden(sessions(s, &ws)));
    assert!(forbidden(users(s, &ws)));
    let t = NewToken {
        name: "x".into(),
        org: Some(acme.clone()),
        expires: None,
        scopes: vec![],
        superadmin: false,
    };
    assert!(forbidden(create_token(s, &ws, t)));
    // But it may ask who it is.
    assert_eq!(me(s, &ws).unwrap()["auth"]["kind"], "workspace");
}

fn new_token(o: &str) -> NewToken {
    NewToken {
        name: "child".into(),
        org: Some(org(o)),
        expires: Some("1h".into()),
        scopes: vec!["read".into()],
        superadmin: false,
    }
}

#[test]
fn tokens_never_mint_tokens() {
    let w = world();
    let s = &w.s;
    // A person signed in mints one, for an org they are in.
    let v = create_token(s, &session(s, &w.member), new_token("acme")).unwrap();
    assert!(v["token"].as_str().unwrap().starts_with("isb_tok_"));
    assert_eq!(v["info"]["scopes"], json!(["read"]));
    assert!(forbidden(create_token(
        s,
        &session(s, &w.outsider),
        new_token("acme")
    )));
    let access = as_kind(s, &w.member, PrincipalKind::Access);
    assert!(create_token(s, &access, new_token("acme")).is_ok());
    // No token of any kind mints one: not narrower, not shorter-lived.
    for scopes in [&[][..], &["admin"][..]] {
        let t = org_token(s, &w.owner, "acme", scopes);
        let e = create_token(s, &t, new_token("acme")).unwrap_err();
        assert!(e.to_string().contains("cannot mint"), "{e}");
    }
    let sa_token = Superadmin::as_user(
        SuperadminSource::Token {
            id: 1,
            name: "ops".into(),
        },
        w.root.clone(),
        s,
    )
    .unwrap();
    assert!(forbidden(create_token(
        s,
        &sa_token.principal,
        new_token("acme")
    )));
    // A tailnet superadmin with an account is a person at a keyboard.
    let tailnet = Superadmin::as_user(
        SuperadminSource::Tailnet {
            login: "root@x.io".into(),
            node: "laptop".into(),
            tags: vec![],
        },
        w.root.clone(),
        s,
    )
    .unwrap();
    assert!(create_token(s, &tailnet.principal, new_token("acme")).is_ok());
    // A superadmin token is never minted here.
    let mut t = new_token("acme");
    t.superadmin = true;
    assert!(forbidden(create_token(s, &session(s, &w.root), t)));
}

#[test]
fn revoking_tokens_and_keys() {
    let w = world();
    let s = &w.s;
    let member = session(s, &w.member);
    let v = create_token(s, &member, new_token("acme")).unwrap();
    let id = v["info"]["id"].as_i64().unwrap();
    // Another member does not see it; an org admin may revoke it.
    assert!(hidden(revoke_token(s, &session(s, &w.viewer), id)));
    assert!(hidden(revoke_token(s, &session(s, &w.outsider), id)));
    revoke_token(s, &session(s, &w.admin), id).unwrap();
    assert!(hidden(revoke_token(s, &member, id)));
    // SSH keys are the caller's own.
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh alice@laptop";
    let k = add_ssh_key(s, &member, key, Some("laptop")).unwrap();
    let kid = k["ssh_key"]["id"].as_i64().unwrap();
    assert_eq!(
        ssh_keys(s, &member).unwrap()["ssh_keys"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        ssh_keys(s, &session(s, &w.viewer)).unwrap()["ssh_keys"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(hidden(delete_ssh_key(s, &session(s, &w.viewer), kid)));
    delete_ssh_key(s, &member, kid).unwrap();
}

#[test]
fn users_are_for_platform_admins() {
    let w = world();
    let s = &w.s;
    assert!(forbidden(users(s, &session(s, &w.owner))));
    let change = UserChange {
        disabled: Some(true),
        platform_admin: None,
    };
    assert!(forbidden(update_user(
        s,
        &session(s, &w.owner),
        w.member.id,
        &change
    )));
    let root = session(s, &w.root);
    assert_eq!(
        users(s, &root).unwrap()["users"].as_array().unwrap().len(),
        6
    );
    // Not yourself, and never the last platform admin.
    assert!(forbidden(update_user(s, &root, w.root.id, &change)));
    let v = update_user(s, &root, w.member.id, &change).unwrap();
    assert_eq!(v["user"]["disabled"], true);
    // A platform admin's org token is confined to the org: not a platform admin.
    let t = as_kind(
        s,
        &w.root,
        PrincipalKind::ApiToken {
            id: 1,
            org: Some(org("acme")),
            name: "t".into(),
            scopes: vec![],
        },
    );
    assert!(forbidden(users(s, &t)));
}
