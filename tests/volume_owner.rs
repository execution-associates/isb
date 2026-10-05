//! A named volume's owner and mode on shell-less OCI images, and a
//! multi-line `sh -c` script as an OCI command line, against a real incusd.
//!
//! Gated by `ISB_INTEGRATION=1`. Everything happens in a throwaway incus
//! project of its own (`isb-test-own-<pid>`), deleted afterwards with all it
//! holds; no org's project is touched.

use std::time::{Duration, Instant};

use isb::spec::VolumeSpec;
use isb::{Client, ReadyCheck, Sandbox, SandboxSpec, Volume};

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;
use common::enabled;

struct Project {
    base: Client,
    name: String,
    client: Client,
    instances: Vec<String>,
    volumes: Vec<String>,
}

impl Drop for Project {
    fn drop(&mut self) {
        for i in &self.instances {
            let _ = Sandbox::remove(&self.client, i, true);
        }
        for v in &self.volumes {
            let _ = isb::volume::remove(&self.client, "default", v);
        }
        let path = format!("/1.0/projects/{}", self.name);
        if let Err(e) = self.base.mutate(
            "DELETE",
            &path,
            None,
            "delete test project",
            Duration::from_secs(60),
        ) {
            eprintln!("cleanup: project {}: {e}", self.name);
        }
    }
}

fn project() -> Project {
    let base = Client::new();
    let name = format!("isb-test-own-{}", std::process::id());
    let body = serde_json::json!({
        "name": name,
        "description": "isb volume_owner integration test (throwaway)",
        "config": {"features.images": "false", "features.profiles": "false",
                   "features.storage.volumes": "true", "user.isb-test": "1"},
    });
    base.mutate(
        "POST",
        "/1.0/projects",
        Some(&body),
        "create test project",
        Duration::from_secs(60),
    )
    .unwrap();
    let client = base.clone().project(&name);
    Project {
        base,
        name,
        client,
        instances: vec![],
        volumes: vec![],
    }
}

fn stat(client: &Client, name: &str, path: &str) -> isb::sftp::Stat {
    let mut s = isb::sftp::Sftp::open(client, name, Duration::from_secs(30)).unwrap();
    s.stat(path)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} missing"))
}

fn spec(name: &str, image: &str, mounts: Vec<(&str, VolumeSpec)>) -> SandboxSpec {
    let mut s = SandboxSpec::new(name, image)
        .label("isb-test", "1")
        .ready(vec![ReadyCheck::Running]);
    for (target, v) in mounts {
        s = s.volume(target, v);
    }
    s
}

#[test]
fn owner_and_mode_without_a_shell_and_multi_line_commands() {
    if !enabled() {
        return;
    }
    let mut p = project();
    let c = p.client.clone();
    let pid = std::process::id();
    let (a, b) = (
        format!("isb-test-{pid}-noshell"),
        format!("isb-test-{pid}-user"),
    );
    p.instances.extend([a.clone(), b.clone()]);
    let (va, vb) = (format!("isb-test-{pid}-va"), format!("isb-test-{pid}-vb"));
    p.volumes.extend([va.clone(), vb.clone()]);

    // traefik/whoami is FROM scratch: no shell, no /etc/passwd. A numeric
    // owner and a mode still land (at creation and through SFTP).
    let s = spec(
        &a,
        "docker:traefik/whoami",
        vec![(
            "/data",
            Volume::named(&va)
                .owner("1000:1000")
                .mode("2750")
                .device("data"),
        )],
    );
    let sb = Sandbox::connect_or_create(&c, &s).expect("create shell-less");
    let st = stat(&c, sb.name(), "/data");
    assert_eq!((st.uid, st.gid, st.mode), (1000, 1000, 0o2750));
    // A user name the image cannot resolve fails naming the step.
    let fix = isb::owner::Fix {
        path: "/data",
        owner: Some("dev"),
        ..Default::default()
    };
    let e = isb::owner::apply(&c, sb.name(), &fix)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("chown dev /data") && e.contains("no such user"),
        "{e}"
    );

    // busybox as uid 1000 with no owner: the new volume is the user's, and a
    // multi-line `sh -c` script runs with its quotes and arguments intact.
    let mut s = spec(
        &b,
        "docker:busybox",
        vec![("/data", Volume::named(&vb).device("data"))],
    );
    s.user = Some("1000:1000".into());
    s.command = Some(
        [
            "sh",
            "-ec",
            "printf '%s|' \"$0\" \"$1\" > /data/out\necho \"it's $((1+1))\" >> /data/out\nexec sleep 600",
            "zero",
            "one",
        ]
        .map(String::from)
        .to_vec(),
    );
    let sb = Sandbox::connect_or_create(&c, &s).expect("create busybox");
    let st = stat(&c, sb.name(), "/data");
    assert_eq!((st.uid, st.gid), (1000, 1000));
    let deadline = Instant::now() + Duration::from_secs(60);
    let out = loop {
        match c.read_file(sb.name(), "/data/out").unwrap() {
            Some(o) if o.ends_with(b"\n") => break String::from_utf8(o).unwrap(),
            _ if Instant::now() > deadline => panic!(
                "no /data/out; console: {}",
                String::from_utf8_lossy(&c.console_log(sb.name()).unwrap_or_default())
            ),
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    };
    assert_eq!(out, "zero|one|it's 2\n");

    // A system image (`dev`, uid 1000, from its /etc/passwd): an owner by
    // name fixes the root-owned parents inside ~dev the mount conjured; the
    // service user takes a new unseeded volume with no owner.
    let (d, vc, vd) = (
        format!("isb-test-{pid}-sys"),
        format!("isb-test-{pid}-vc"),
        format!("isb-test-{pid}-vd"),
    );
    p.instances.push(d.clone());
    p.volumes.extend([vc.clone(), vd.clone()]);
    let mut s = spec(
        &d,
        &common::image(),
        vec![
            (
                "/home/dev/.cache/isbtest/data",
                Volume::named(&vc).owner("dev").device("c"),
            ),
            ("/srv/state", Volume::named(&vd).device("d")),
        ],
    );
    s.user = Some("dev".into());
    let sb = Sandbox::connect_or_create(&c, &s).expect("create system container");
    for path in [
        "/home/dev/.cache/isbtest/data",
        "/home/dev/.cache/isbtest",
        "/srv/state",
    ] {
        assert_eq!(stat(&c, sb.name(), path).uid, 1000, "{path}");
    }
    assert_eq!(stat(&c, sb.name(), "/srv").uid, 0);
}
