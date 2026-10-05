//! Tests of fleet servers: call placement, the agent and its certificate,
//! forwarded calls and their arguments.

use super::*;
use crate::auth::Role;
use crate::server::{Shutdown, serve_until};
use crate::servers::AgentClient;
use crate::servers::pki::{self, Ca};
use crate::servers::store::AgentOrgs;

fn org(o: &str) -> OrgId {
    OrgId::new(o).unwrap()
}

#[test]
fn calls_go_where_their_org_lives() {
    let placed = |o: &OrgId| (o.as_str() == "far").then(|| "box".to_string());
    let d = |t: &str, a: Value| decide(t, &a, &placed);
    assert_eq!(
        d("stack_deploy", json!({"org": "far"})),
        Way::Forward("box".into(), org("far"))
    );
    assert_eq!(d("stack_deploy", json!({"org": "near"})), Way::Here);
    assert_eq!(
        d("stack_deploy", json!({})),
        Way::Here,
        "the default org is local"
    );
    assert_eq!(
        d("secret_set", json!({"org": "far"})),
        Way::Forward("box".into(), org("far"))
    );
    assert_eq!(
        d("app_deploy", json!({"org": "Bad!"})),
        Way::Here,
        "the tool refuses it"
    );
    for t in ["overview", "stack_list", "ingress_status", "org_list"] {
        assert_eq!(d(t, json!({"org": "far"})), Way::FanOut, "{t}");
    }
    for t in [
        "events",
        "audit_list",
        "server_status",
        "server_add",
        "template_list",
        "registry_gc",
    ] {
        assert_eq!(d(t, json!({"org": "far"})), Way::Here, "{t}");
    }
    assert_eq!(
        d("secret_reencrypt", json!({"org": "far", "all": true})),
        Way::Here
    );
    assert_eq!(
        d("secret_reencrypt", json!({"org": "far"})),
        Way::Forward("box".into(), org("far"))
    );
    assert_eq!(
        d("org_create", json!({"org": "x", "server": "box"})),
        Way::OrgCreate("box".into())
    );
    assert_eq!(
        d("org_create", json!({"org": "x", "server": "local"})),
        Way::Here
    );
    assert_eq!(d("org_create", json!({"org": "x"})), Way::Here);
    assert_eq!(
        d(
            "org_create",
            json!({"org": "x", "placement": {"server": "box"}})
        ),
        Way::OrgCreate("box".into())
    );
    assert_eq!(
        d(
            "org_create",
            json!({"org": "x", "placement": {"vm": {"cpus": 4}}})
        ),
        Way::OrgCreateVm(vm::VmSize {
            cpus: 4,
            memory: "4GiB".into(),
            disk: "40GiB".into()
        })
    );
    assert_eq!(
        d("org_create", json!({"org": "x", "placement": "local"})),
        Way::Here
    );
    assert_eq!(
        d(
            "org_create",
            json!({"org": "x", "placement": {"vm": {"cpus": 0}}})
        ),
        Way::Here,
        "the local tool refuses a bad placement"
    );
    assert_eq!(d("org_delete", json!({"org": "far"})), Way::OrgOther);
}

/// An agent's mTLS listener on loopback with one tool, `echo`, that
/// answers its arguments and caller; orgs `acme` and `gamma` placed.
struct TestAgent {
    port: u16,
    ca: Ca,
    stop: Shutdown,
    _dir: tempfile::TempDir,
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        self.stop.trigger();
    }
}

fn agent() -> TestAgent {
    agent_with(None)
}

fn agent_with(ssh: Option<crate::server::ssh::Ssh>) -> TestAgent {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::open(&dir.path().join("pki")).unwrap();
    let leaf = ca.issue_server("box", "127.0.0.1").unwrap();
    let tls = Arc::new(std::sync::RwLock::new(
        pki::server_config(&ca.cert_pem, &leaf).unwrap(),
    ));
    let orgs = Arc::new(AgentOrgs::open(dir.path()).unwrap());
    orgs.set(&org("acme"), true).unwrap();
    orgs.set(&org("gamma"), true).unwrap();
    let mut r = Registry::new();
    r.register(Tool::new("echo", "Echo", json!({}), |a, c| {
        Ok(json!({"args": a, "caller": c.to_string()}))
    }))
    .unwrap();
    // Sessions are admitted as this would be.
    r.register(Tool::new("sandbox_exec", "Exec", json!({}), |_, _| {
        Ok(Value::Null)
    }))
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let l = Listener::mtls(format!("127.0.0.1:{port}"), tls).hooks(agent_hooks(
        &Hooks::default(),
        orgs,
        ssh,
    ));
    let stop = Shutdown::new();
    let s2 = stop.clone();
    std::thread::spawn(move || {
        serve_until(vec![l], r, Arc::new(|| (true, json!({"ok": true}))), s2)
    });
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    TestAgent {
        port,
        ca,
        stop,
        _dir: dir,
    }
}

fn member_of_acme() -> Assertion {
    Assertion::for_caller(&super::super::tests::token(&[("acme", Role::Member)], &[])).unwrap()
}

#[test]
fn the_agent_takes_only_the_control_planes_certificate() {
    let a = agent();
    let ok = AgentClient::new("box", "127.0.0.1", a.port, a.ca.client_config().unwrap());
    let v = ok
        .call(
            "echo",
            &json!({}),
            &member_of_acme(),
            Some(&org("acme")),
            None,
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(v["args"]["org"], "acme");
    assert_eq!(ok.peer_fingerprint().unwrap().len(), 64);

    let try_with = |cfg: Arc<rustls::ClientConfig>| {
        AgentClient::new("box", "127.0.0.1", a.port, cfg).call(
            "echo",
            &json!({}),
            &member_of_acme(),
            Some(&org("acme")),
            None,
            Duration::from_secs(10),
        )
    };
    // Another CA's client certificate.
    let d2 = tempfile::tempdir().unwrap();
    let other = Ca::open(d2.path()).unwrap();
    let foreign = pki::client_config(&a.ca.cert_pem, &other.client().unwrap()).unwrap();
    assert!(
        try_with(foreign).is_err(),
        "a foreign client certificate is refused"
    );
    // No client certificate at all.
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls::pki_types::CertificateDer::pem_slice_iter(a.ca.cert_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    use rustls::pki_types::pem::PemObject;
    assert!(
        try_with(crate::notify::net::tls_with_roots(roots)).is_err(),
        "no client certificate"
    );
    // An agent's own (serverAuth) certificate, from the right CA.
    let agent_leaf = a.ca.issue_server("other", "127.0.0.1").unwrap();
    let as_agent = pki::client_config(&a.ca.cert_pem, &agent_leaf).unwrap();
    assert!(
        try_with(as_agent).is_err(),
        "a server certificate cannot act as the control plane"
    );
    // And the control plane checks the agent: one from another CA fails.
    let wrong_ca = pki::client_config(&other.cert_pem, &a.ca.client().unwrap()).unwrap();
    assert!(
        try_with(wrong_ca).is_err(),
        "an agent with another CA's certificate"
    );
}

#[test]
fn rotation_writes_the_new_leaf_and_swaps_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::open(&dir.path().join("pki")).unwrap();
    let tls_dir = dir.path().join("tls");
    std::fs::create_dir_all(&tls_dir).unwrap();
    std::fs::write(tls_dir.join(pki::AGENT_CA), &ca.cert_pem).unwrap();
    let old = ca.issue_server("box", "127.0.0.1").unwrap();
    let cfg = pki::server_config(&ca.cert_pem, &old).unwrap();
    let st = AgentState {
        orgs: Arc::new(AgentOrgs::open(dir.path()).unwrap()),
        tls: Arc::new(std::sync::RwLock::new(cfg.clone())),
        tls_dir: tls_dir.clone(),
    };
    let new = ca.issue_server("box", "127.0.0.1").unwrap();
    let body = serde_json::to_vec(&json!({"cert": new.cert, "key": new.key})).unwrap();
    assert_eq!(rotate(&st, &body).unwrap(), new.fingerprint().unwrap());
    assert_eq!(
        std::fs::read_to_string(tls_dir.join(pki::AGENT_CERT)).unwrap(),
        new.cert
    );
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::metadata(tls_dir.join(pki::AGENT_KEY)).unwrap();
    assert_eq!(m.permissions().mode() & 0o777, 0o600);
    assert!(
        !Arc::ptr_eq(&st.tls.read().unwrap(), &cfg),
        "new connections get the new config"
    );
    let bad = serde_json::to_vec(&json!({"cert": "nope", "key": new.key})).unwrap();
    assert!(rotate(&st, &bad).is_err());
    assert_eq!(
        std::fs::read_to_string(tls_dir.join(pki::AGENT_CERT)).unwrap(),
        new.cert,
        "a bad one changes nothing"
    );
}

/// An SSH session's far end: says which key sshd took, then echoes.
struct EchoSsh {
    noted: bool,
    pending: Vec<u8>,
}

impl crate::server::terminal::Pty for EchoSsh {
    fn input(&mut self, data: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(data);
        Ok(())
    }
    fn resize(&mut self, _: u16, _: u16) {}
    fn output(&mut self, wait: Duration) -> crate::server::terminal::PtyOutput {
        use crate::server::terminal::PtyOutput;
        if !self.noted {
            self.noted = true;
            return PtyOutput::Note(
                json!({"type": "accepted", "user": "dev", "fingerprint": "SHA256:k"}),
            );
        }
        if !self.pending.is_empty() {
            return PtyOutput::Data(std::mem::take(&mut self.pending));
        }
        std::thread::sleep(wait.min(Duration::from_millis(10)));
        PtyOutput::Idle
    }
    fn close(&mut self) {}
}

#[test]
fn a_forwarded_ssh_session_carries_the_keys_and_says_which_key_sshd_took() {
    use crate::server::ssh::SshRequest;
    use crate::server::terminal::PtyOutput;
    type Seen = Option<(String, String, Option<Vec<String>>)>;
    let seen: Arc<std::sync::Mutex<Seen>> = Arc::default();
    let s2 = seen.clone();
    let hook: crate::server::ssh::Ssh = Arc::new(move |_c, o, s| {
        *s2.lock().unwrap() = Some((o.to_string(), s.instance.clone(), s.forwarded_keys.clone()));
        Ok(Box::new(EchoSsh {
            noted: false,
            pending: Vec::new(),
        }) as Box<dyn crate::server::terminal::Pty>)
    });
    let a = agent_with(Some(hook));
    let c = AgentClient::new("box", "127.0.0.1", a.port, a.ca.client_config().unwrap());
    let req = SshRequest {
        instance: "box".into(),
        keys_of: None,
        forwarded_keys: None,
    };
    let keys = vec!["ssh-ed25519 AAAAC3Nza me".to_string()];
    let mut p = c.ssh(&member_of_acme(), &org("acme"), &req, &keys).unwrap();
    let mut next = || {
        for _ in 0..200 {
            match p.output(Duration::from_millis(20)) {
                PtyOutput::Idle => {}
                o => return o,
            }
        }
        PtyOutput::Idle
    };
    match next() {
        PtyOutput::Note(v) => assert_eq!(v["fingerprint"], "SHA256:k"),
        o => panic!("{o:?}"),
    }
    p.input(b"ping").unwrap();
    let mut got = Vec::new();
    for _ in 0..200 {
        if let PtyOutput::Data(d) = p.output(Duration::from_millis(20)) {
            got.extend(d);
            if got.len() >= 4 {
                break;
            }
        }
    }
    assert_eq!(got, b"ping");
    p.close();
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(("acme".into(), "box".into(), Some(keys.clone())))
    );
    // An org not placed here: refused before any session.
    let e = c
        .ssh(&Assertion::control_plane(), &org("beta"), &req, &keys)
        .err()
        .unwrap();
    assert!(matches!(e, Error::Forbidden(_)), "{e}");
}

#[test]
fn a_forwarded_call_stays_in_its_org() {
    let a = agent();
    let c = AgentClient::new("box", "127.0.0.1", a.port, a.ca.client_config().unwrap());
    let call = |args: Value, who: &Assertion, o: &str| {
        c.call(
            "echo",
            &args,
            who,
            Some(&org(o)),
            None,
            Duration::from_secs(10),
        )
    };
    let m = member_of_acme();
    // A forged org in the arguments of a call for acme.
    let e = call(json!({"org": "gamma"}), &m, "acme").unwrap_err();
    assert!(
        matches!(e, Error::Forbidden(ref s) if s.contains("acts in org acme")),
        "{e}"
    );
    // An org the caller is not in, though it is placed here.
    let e = call(json!({}), &m, "gamma").unwrap_err();
    assert!(matches!(e, Error::Forbidden(_)), "{e}");
    // An org not placed here, even for a platform admin.
    let e = call(json!({}), &Assertion::control_plane(), "beta").unwrap_err();
    assert!(
        matches!(e, Error::Forbidden(ref s) if s.contains("not placed")),
        "{e}"
    );
    assert!(call(json!({}), &Assertion::control_plane(), "gamma").is_ok());
    // No assertion: refused.
    let r = c
        .request(
            "POST",
            "/orgs/acme/api/v1/tools/echo",
            &[],
            b"{}",
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(r.status, 401);
    // A garbled one: refused too.
    let r = c
        .request(
            "POST",
            "/orgs/acme/api/v1/tools/echo",
            &[("Authorization".into(), "IsbAssert e30".into())],
            b"{}",
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(r.status, 401);
}

#[test]
fn which_calls_are_org_bound_on_an_agent() {
    assert!(org_bound("stack_deploy", true));
    assert!(org_bound("secret_get", false));
    assert!(org_bound("org_create", false));
    assert!(!org_bound("overview", false));
    assert!(org_bound("overview", true));
    assert!(!org_bound("server_status", false));
    assert!(!org_bound("org_list", false));
}

#[test]
fn a_resolved_compose_file_goes_as_text_and_host_paths_stay_home() {
    let a = json!({"name": "s", "file": {"services": {"w": {"image": "x", "command": "echo $HOME"}}}, "base_dir": "/home/me/p", "project": "shop", "environment": "staging"});
    let f = forwarded_args("stack_deploy", a.clone(), &Caller::Local { uid: None }).unwrap();
    assert!(f.get("file").is_none() && f.get("base_dir").is_none());
    // The owner travels as given.
    assert_eq!(
        (f["project"].as_str(), f["environment"].as_str()),
        (Some("shop"), Some("staging"))
    );
    assert!(f["compose"].as_str().unwrap().contains("$$HOME"));
    let u = forwarded_args("stack_logs", a, &super::super::tests::token(&[], &[])).unwrap();
    assert_eq!(
        u["base_dir"], "/home/me/p",
        "a remote caller's base_dir is the agent's to judge"
    );
}
