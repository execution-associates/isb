use super::*;
use crate::auth::tests::store;
use crate::auth::webauthn::Registration;

const PW: &str = "correct horse battery";

fn ext(sub: &str, email: Option<&str>, verified: bool) -> ExternalIdentity {
    ExternalIdentity {
        provider: "github".into(),
        subject: sub.into(),
        email: email.map(str::to_string),
        email_verified: verified,
        name: Some("Gina Hub".into()),
    }
}

fn code(e: AuthError) -> &'static str {
    match e {
        AuthError::Refused { code, .. } => code,
        e => panic!("not a refusal: {e:?}"),
    }
}

fn reg(id: u8) -> Registration {
    Registration {
        credential_id: vec![id; 16],
        public_key_cose: vec![0xa0],
        alg: -7,
        sign_count: 0,
        aaguid: [0; 16],
        user_verified: true,
    }
}

#[test]
fn sign_in_rules() {
    let (s, _) = store();
    let e = s.external_sign_in(&ext("1", Some("a@x.io"), true), None, false);
    assert_eq!(code(e.unwrap_err()), "setup_required");
    let root = s.create_first_admin("root@x.io", "Root", PW).unwrap();

    // A verified email matching a user links and signs in; then the
    // identity alone signs in, whatever email it later reports.
    let (u, how) = s
        .external_sign_in(&ext("1", Some("ROOT@x.io"), true), None, false)
        .unwrap();
    assert_eq!((u.id, how), (root.id, SignIn::Linked));
    let (u, how) = s
        .external_sign_in(&ext("1", Some("other@x.io"), false), None, true)
        .unwrap();
    assert_eq!((u.id, how), (root.id, SignIn::Existing));
    let ids = s.list_identities(root.id).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].email.as_deref(), Some("other@x.io"));
    assert!(!ids[0].email_verified);

    // An unverified email never links, even to an existing user, and never
    // signs up, even with open sign-up.
    let e = s.external_sign_in(&ext("2", Some("root@x.io"), false), None, true);
    assert_eq!(code(e.unwrap_err()), "unverified_email");
    let e = s.external_sign_in(&ext("2", None, true), None, true);
    assert_eq!(code(e.unwrap_err()), "unverified_email");
    assert_eq!(s.list_identities(root.id).unwrap().len(), 1);

    // No account, no invitation: closed.
    let e = s.external_sign_in(&ext("3", Some("new@x.io"), true), None, false);
    assert_eq!(code(e.unwrap_err()), "signup_closed");
    assert!(s.user_by_email("new@x.io").unwrap().is_none());

    // Open sign-up makes an account without a password or orgs.
    let (u, how) = s
        .external_sign_in(&ext("3", Some("new@x.io"), true), None, true)
        .unwrap();
    assert_eq!(how, SignIn::Created);
    assert_eq!(u.name, "Gina Hub");
    assert!(!u.has_password && !u.platform_admin);
    assert!(s.memberships(u.id).unwrap().is_empty());

    // A disabled user's identity signs nobody in.
    s.set_disabled(u.id, true).unwrap();
    let e = s.external_sign_in(&ext("3", Some("new@x.io"), true), None, true);
    assert_eq!(code(e.unwrap_err()), "account_disabled");
}

#[test]
fn sign_up_by_invitation() {
    let (s, _) = store();
    s.create_first_admin("root@x.io", "Root", PW).unwrap();
    let ocai = OrgId::new("ocai").unwrap();
    let lab = OrgId::new("lab").unwrap();
    let inv = s
        .create_invitation(None, &ocai, "inv@x.io", Role::Admin)
        .unwrap();
    s.create_invitation(None, &lab, "inv@x.io", Role::Member)
        .unwrap();
    let other = s
        .create_invitation(None, &ocai, "someone@x.io", Role::Member)
        .unwrap();

    // A token for another address is refused; so is a made-up one.
    let e = s.external_sign_in(&ext("9", Some("inv@x.io"), true), Some(&other.token), false);
    assert_eq!(code(e.unwrap_err()), "invitation_mismatch");
    let bogus = format!("isb_inv_{}", "A".repeat(43));
    assert!(matches!(
        s.external_sign_in(&ext("9", Some("inv@x.io"), true), Some(&bogus), false),
        Err(AuthError::InvalidToken(_))
    ));

    // With the right token (or none: the pending invitation is enough),
    // the account is made and every pending invitation accepted.
    let (u, how) = s
        .external_sign_in(&ext("9", Some("Inv@X.io"), true), Some(&inv.token), false)
        .unwrap();
    assert_eq!(how, SignIn::Created);
    assert_eq!(u.email, "inv@x.io");
    let m = s.memberships(u.id).unwrap();
    assert_eq!(m.len(), 2);
    assert!(m.iter().any(|m| m.org == ocai && m.role == Role::Admin));
    assert!(m.iter().any(|m| m.org == lab && m.role == Role::Member));
    assert!(s.invitation(&inv.token).unwrap().is_none(), "used up");
    assert!(
        s.list_invitations(&ocai)
            .unwrap()
            .iter()
            .all(|i| i.email != "inv@x.io")
    );

    let (_, how) = s
        .external_sign_in(&ext("10", Some("someone@x.io"), true), None, false)
        .unwrap();
    assert_eq!(how, SignIn::Created);
}

#[test]
fn linking_and_the_last_way_in() {
    let (s, _) = store();
    let root = s.create_first_admin("root@x.io", "Root", PW).unwrap();
    let (u, _) = s
        .external_sign_in(&ext("7", Some("solo@x.io"), true), None, true)
        .unwrap();
    // Someone else's identity cannot be linked.
    let e = s
        .link_identity(root.id, &ext("7", None, false))
        .unwrap_err();
    assert_eq!(code(e), "identity_taken");
    // Linking needs no email: the user proved both sides.
    let g = ExternalIdentity {
        provider: "google".into(),
        ..ext("g-1", None, false)
    };
    let linked = s.link_identity(root.id, &g).unwrap();
    assert_eq!(linked.provider, "google");
    assert!(s.link_identity(root.id, &g).is_ok(), "relinking is a no-op");

    // u has one way in: its GitHub identity.
    let only = s.list_identities(u.id).unwrap()[0].id;
    assert!(matches!(
        s.unlink_identity(u.id, only),
        Err(AuthError::Conflict(_))
    ));
    // Not someone else's.
    assert!(!s.unlink_identity(root.id, only).unwrap());
    // With a passkey too, either can go, but not both.
    let pk = s
        .add_passkey(
            u.id,
            b"handle",
            &reg(1),
            " Laptop\n",
            &["internal".into(), "bad transport!".into()],
        )
        .unwrap();
    assert_eq!(pk.name, "Laptop");
    assert_eq!(pk.transports, vec!["internal".to_string()]);
    assert!(s.unlink_identity(u.id, only).unwrap());
    assert!(matches!(
        s.delete_passkey(u.id, pk.id),
        Err(AuthError::Conflict(_))
    ));
    // A password counts as a way in.
    s.set_password(u.id, PW).unwrap();
    assert!(s.delete_passkey(u.id, pk.id).unwrap());
    assert!(!s.delete_passkey(u.id, pk.id).unwrap());
}

#[test]
fn passkey_store() {
    let (s, _) = store();
    let root = s.create_first_admin("root@x.io", "Root", PW).unwrap();
    assert_eq!(s.passkey_user_handle(root.id).unwrap(), None);
    let pk = s.add_passkey(root.id, b"h1", &reg(1), "", &[]).unwrap();
    assert_eq!(
        s.passkey_user_handle(root.id).unwrap().as_deref(),
        Some(&b"h1"[..])
    );
    assert!(matches!(
        s.add_passkey(root.id, b"h1", &reg(1), "", &[]),
        Err(AuthError::Conflict(_))
    ));
    let found = s.passkey_by_credential(&[1; 16]).unwrap().unwrap();
    assert_eq!(found.passkey.id, pk.id);
    assert_eq!(found.user_handle, b"h1");
    assert_eq!(found.passkey.aaguid, "0".repeat(32));
    s.use_passkey(pk.id, 0, 5).unwrap();
    // A stale counter (a racing or replayed assertion) is refused.
    assert!(matches!(
        s.use_passkey(pk.id, 0, 6),
        Err(AuthError::PasskeyRejected(_))
    ));
    let l = s.list_passkeys(root.id).unwrap();
    assert_eq!(l[0].sign_count, 5);
    assert!(l[0].last_used.is_some());
    assert!(s.passkey_by_credential(&[2; 16]).unwrap().is_none());
}
