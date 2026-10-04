//! A stack's UDP port against a real incusd (docs/concepts/stacks.md#udp-ports):
//! a NAT proxy on the service's one replica that keeps the client's address,
//! follows a redeploy, is refused where the org was not allowed it, and is
//! the only proxy device the org's project admits.
//!
//! Gated like the other integration tests (`ISB_INTEGRATION=1`; build them
//! in a sandbox with `cargo test --no-run`, run the binary on the host). It
//! makes an org `isbtest-u<pid>` (the host needs `sudo isb host setup` for
//! DHCP on org bridges) and publishes on the host's own address
//! (`ISB_TEST_UDP_ADDR`, default the source address of the default route),
//! sending from the host itself, so no firewall rule is needed. Everything
//! is removed afterwards, pass or fail.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use isb::Client;
use isb::org::{OrgId, OrgOptions};
use isb::stack::{Controller, StackDef};

fn enabled() -> bool {
    if std::env::var("ISB_INTEGRATION").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set ISB_INTEGRATION=1 to run against incusd");
    false
}

fn image() -> String {
    std::env::var("ISB_TEST_IMAGE").unwrap_or_else(|_| "dev-base".into())
}

/// The host address to publish on.
fn host_addr() -> IpAddr {
    if let Ok(a) = std::env::var("ISB_TEST_UDP_ADDR") {
        return a.parse().unwrap();
    }
    // Connecting a UDP socket sends nothing; it only picks the source.
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.connect("192.0.2.1:9").unwrap();
    s.local_addr().unwrap().ip()
}

fn free_udp_port(ip: IpAddr) -> u16 {
    UdpSocket::bind(SocketAddr::new(ip, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The echo server's answer: `<hostname> <the source address it saw>`.
fn ask(to: SocketAddr, deadline: Duration) -> Option<String> {
    let s = UdpSocket::bind(SocketAddr::new(to.ip(), 0)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let start = Instant::now();
    let mut buf = [0u8; 256];
    while start.elapsed() < deadline {
        let _ = s.send_to(b"ping", to);
        if let Ok((n, _)) = s.recv_from(&mut buf) {
            return Some(String::from_utf8_lossy(&buf[..n]).into_owned());
        }
    }
    None
}

const ECHO: &str = "import socket\n\
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\n\
s.bind(('0.0.0.0', 7000))\n\
h = socket.gethostname()\n\
while True:\n\
\x20   d, a = s.recvfrom(256)\n\
\x20   s.sendto((h + ' ' + a[0]).encode(), a)\n";

fn def(
    org: &OrgId,
    name: &str,
    listen: SocketAddr,
    replicas: u32,
    dir: &std::path::Path,
) -> StackDef {
    // JSON is YAML.
    let file = serde_json::json!({"services": {"echo": {
        "image": image(),
        "cpus": 1,
        "mem_limit": "512MiB",
        "command": ["python3", "-c", ECHO],
        "ports": [format!("{}:{}:7000/udp", listen.ip(), listen.port())],
        "deploy": {"replicas": replicas},
    }}});
    let p = isb::compose::load_docs(
        &[(dir.join("isb.yaml"), file.to_string())],
        dir,
        Some(name),
        &|_| None,
    )
    .unwrap();
    StackDef {
        name: name.into(),
        org: org.clone(),
        file: p.file,
        base_dir: dir.to_path_buf(),
        secrets: Default::default(),
        force: Default::default(),
        images: Default::default(),
        deployed_at: 0,
        deployed_by: "test".into(),
        previous: None,
    }
}

/// The replica's devices, by name.
fn devices(client: &Client, org: &OrgId, instance: &str) -> serde_json::Value {
    isb::org::client(client, org)
        .get(&format!("/1.0/instances/{instance}"))
        .unwrap()["devices"]
        .clone()
}

/// The org's project allows proxy devices once it has a UDP port; isb still
/// refuses any but a stack's UDP port, here a sandbox's own TCP port.
fn org_admits_only_stack_udp(client: &Client, org: &OrgId, listen: SocketAddr) {
    assert_eq!(
        isb::org::get(client, org).unwrap().udp,
        vec![listen.to_string()]
    );
    let oc = isb::org::client(client, org);
    let project = client
        .get(&format!("/1.0/projects/{}", org.incus_project()))
        .unwrap();
    assert_eq!(project["config"]["restricted.devices.proxy"], "allow");

    let spec = isb::SandboxSpec::new("isb-test-udp-sb", image()).port(isb::PortBinding::host(
        format!("tcp:127.0.0.1:{}", free_udp_port(listen.ip())),
        "tcp:127.0.0.1:80",
    ));
    let e = isb::Sandbox::create(&oc, &spec).unwrap_err();
    assert!(e.to_string().contains("proxy device"), "{e}");
}

/// Refused at deploy: more than one replica, a port the org was not
/// allowed, start-first updates.
fn refusals(
    ctl: &Controller,
    org: &OrgId,
    listen: SocketAddr,
    other: SocketAddr,
    dir: &std::path::Path,
) {
    let e = ctl
        .deploy(def(org, "isb-test-udp2", listen, 2, dir))
        .unwrap_err();
    assert!(e.to_string().contains("at most one replica"), "{e}");
    let e = ctl
        .deploy(def(org, "isb-test-udpx", other, 1, dir))
        .unwrap_err();
    assert!(e.to_string().contains("may not publish UDP"), "{e}");
    let mut sf = def(org, "isb-test-udpsf", listen, 1, dir);
    let svc = sf.file.services.get_mut("echo").unwrap();
    svc.deploy.as_mut().unwrap().update_config = Some(isb::spec::UpdateConfig {
        order: Some(isb::spec::UpdateOrder::StartFirst),
        ..Default::default()
    });
    let e = ctl.deploy(sf).unwrap_err();
    assert!(e.to_string().contains("stop-first"), "{e}");
}

#[test]
fn stack_udp_port() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let n = std::process::id() % 100000;
    let org = OrgId::new(format!("isbtest-u{n}")).unwrap();
    let ip = host_addr();
    let listen = SocketAddr::new(ip, free_udp_port(ip));
    let other = SocketAddr::new(ip, free_udp_port(ip));

    struct Rm(Client, OrgId, Option<Controller>);
    impl Drop for Rm {
        fn drop(&mut self) {
            if let Some(ctl) = self.2.take() {
                for s in ctl.definitions() {
                    let _ = ctl.remove(&s.qualified(), true, Duration::from_secs(120));
                }
                ctl.shutdown();
            }
            let _ = isb::org::remove(&self.0, &self.1, true, &mut |_| {});
        }
    }
    let mut rm = Rm(client.clone(), org.clone(), None);
    isb::org::ensure(
        &client,
        &org,
        &OrgOptions {
            cpus: Some(4),
            memory: Some("4GiB".into()),
            udp: Some(vec![listen]),
            ..Default::default()
        },
        &mut |l| eprintln!("{l}"),
    )
    .unwrap();
    org_admits_only_stack_udp(&client, &org, listen);

    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state.path(),
        std::sync::Arc::new(k),
    )));
    let ctl = Controller::start(client.clone(), store, Duration::from_secs(2), secrets).unwrap();
    rm.2 = Some(ctl.clone());

    refusals(&ctl, &org, listen, other, state.path());

    let name = "isb-test-udp";
    let q = format!("{org}/{name}");
    ctl.deploy(def(&org, name, listen, 1, state.path()))
        .unwrap();
    let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    let port = &st.services[0].ports[0];
    assert_eq!(port.listen, format!("{listen}/udp"));
    let d = devices(&client, &org, &st.services[0].instances[0].name);
    let dev = &d[format!("port-host-udp-{}", listen.port())];
    assert_eq!(dev["nat"], "true", "{d}");
    assert_eq!(dev["connect"], "udp:0.0.0.0:7000", "{d}");

    // The packet reaches the replica with the host's own address as its
    // source: DNAT, not a proxy.
    let first = ask(listen, Duration::from_secs(60)).expect("no answer through the port");
    let (host1, seen) = first.split_once(' ').unwrap();
    assert_eq!(seen, ip.to_string(), "{first}");

    // Refused: another stack on the same port, a second replica.
    let e = ctl
        .deploy(def(&org, "isb-test-udp3", listen, 1, state.path()))
        .unwrap_err();
    assert!(e.to_string().contains("is published by"), "{e}");
    assert!(ctl.scale(&q, "echo", 2).is_err());

    // A redeploy moves the port to the new replica.
    ctl.redeploy(&q, "echo").unwrap();
    let deadline = Instant::now() + Duration::from_secs(300);
    let host2 = loop {
        assert!(
            Instant::now() < deadline,
            "the port did not follow the redeploy"
        );
        if let Some(a) = ask(listen, Duration::from_secs(2)) {
            let h = a.split_once(' ').unwrap().0.to_string();
            if h != host1 {
                break h;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
    let inst = &st.services[0].instances[0];
    assert_eq!(inst.name, host2);
    assert_eq!(
        st.services[0].ports[0].backends,
        vec![format!("{}:7000", inst.ip.as_deref().unwrap())]
    );

    // Removing the stack frees the port: nothing answers.
    ctl.remove(&q, true, Duration::from_secs(120)).unwrap();
    assert!(ask(listen, Duration::from_secs(3)).is_none());
}
