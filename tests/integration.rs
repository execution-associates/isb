//! Integration tests against a real incusd.
//!
//! Gated: they run only with `ISB_INTEGRATION=1`, since the incus socket is
//! root-equivalent. Every instance and volume they create is named `isb-test-*`
//! and removed afterwards, pass or fail; nothing else is touched.
//!
//! `ISB_TEST_IMAGE` picks the image (default `dev-base`: a local image with a
//! `dev` user at uid 1000, python3 and getent). `ISB_BIN` points at the `isb`
//! binary for the CLI tests (default: the one cargo built).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use isb::plan::DiffOptions;
use isb::sandbox::{self, EnsureOptions, LabelFilter};
use isb::spec::{IdmapMode, IdmapSpec};
use isb::{
    Client, Error, ExecEvent, ExecOptions, PortBinding, ReadyCheck, Sandbox, SandboxSpec, Stdin,
    Timeouts, Volume,
};

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

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A unique `isb-test-*` name.
fn test_name(what: &str) -> String {
    format!(
        "isb-test-{}-{}-{what}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    )
}

/// Deletes the instances and volumes it tracks when dropped.
struct Cleanup {
    client: Client,
    instances: Vec<String>,
    volumes: Vec<(String, String)>,
}

impl Cleanup {
    fn new(client: &Client) -> Self {
        Cleanup {
            client: client.clone(),
            instances: vec![],
            volumes: vec![],
        }
    }
    fn instance(&mut self, n: &str) {
        assert!(n.starts_with("isb-test-"));
        self.instances.push(n.into());
    }
    fn volume(&mut self, pool: &str, n: &str) {
        assert!(n.starts_with("isb-test-"));
        self.volumes.push((pool.into(), n.into()));
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for n in &self.instances {
            match Sandbox::remove(&self.client, n, true) {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("cleanup: {n}: {e}"),
            }
        }
        for (p, n) in &self.volumes {
            match isb::volume::remove(&self.client, p, n) {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("cleanup: volume {n}: {e}"),
            }
        }
    }
}

fn tempdir() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("isb-test-")
        .tempdir()
        .unwrap();
    // incusd (root) resolves the bind source; the guest's dev user writes to it.
    std::fs::set_permissions(
        d.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    d
}

fn base_spec(name: &str) -> SandboxSpec {
    SandboxSpec::new(name, image())
        .cpus(2)
        .memory("1GiB")
        .idmap(IdmapSpec::Mode(IdmapMode::Auto))
        .label("isb-test", "1")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn isb_bin() -> PathBuf {
    std::env::var_os("ISB_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_isb")))
}

const WATCHER: &str = r#"
import ctypes, os, struct, sys
libc = ctypes.CDLL(None, use_errno=True)
fd = libc.inotify_init()
if libc.inotify_add_watch(fd, sys.argv[1].encode(), 0x100 | 0x8) < 0:
    sys.exit("inotify_add_watch failed")
print("ready", flush=True)
while True:
    buf = os.read(fd, 4096)
    i = 0
    while i < len(buf):
        _wd, _mask, _cookie, ln = struct.unpack_from("iIII", buf, i)
        name = buf[i + 16:i + 16 + ln].rstrip(b"\0").decode()
        print("event " + name, flush=True)
        if name == "stop":
            sys.exit(0)
        i += 16 + ln
"#;

fn wait_for(watch: &mut isb::ExecStream, needle: &str, lines: &mut String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !lines.contains(needle) {
        assert!(Instant::now() < deadline, "no {needle:?} in {lines:?}");
        match watch.next_event() {
            Some(ExecEvent::Stdout(b)) => lines.push_str(&String::from_utf8_lossy(&b)),
            Some(ExecEvent::Stderr(b)) => eprint!("{}", String::from_utf8_lossy(&b)),
            None => panic!("watcher ended: {lines:?}"),
        }
    }
}

/// Create, readiness, a no-op ensure that provably does not remount (a live
/// inotify watch keeps working), then drift that is repaired.
#[test]
fn create_ready_and_noop_ensure_keeps_watches() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let name = test_name("ensure");
    cleanup.instance(&name);
    let web = tempdir();
    let spec = base_spec(&name)
        .volume(
            "/mnt/web",
            Volume::bind(web.path().to_str().unwrap()).device("web"),
        )
        .user("dev")
        .ready(vec![
            ReadyCheck::Running,
            ReadyCheck::DefaultRoute,
            ReadyCheck::UserExists("dev".into()),
            ReadyCheck::PathWritable("/mnt/web".into()),
        ]);

    let sb = Sandbox::create(&client, &spec).expect("create");
    assert!(sb.info().unwrap().status.eq_ignore_ascii_case("running"));
    assert!(matches!(
        Sandbox::create(&client, &spec),
        Err(Error::AlreadyExists(_))
    ));

    // The bind mount is writable by dev (idmap: auto did its job on this host).
    let out = sb
        .exec(["sh", "-c", "echo hi > /mnt/web/from-guest && id -u"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr_text());
    assert_eq!(out.stdout_text().trim(), "1000");
    assert_eq!(
        std::fs::read_to_string(web.path().join("from-guest")).unwrap(),
        "hi\n"
    );

    // Hold an inotify watch on the mount, as a dev server would.
    let mut watch = sb
        .exec_stream(
            ["python3", "-c", WATCHER, "/mnt/web"],
            ExecOptions::default(),
        )
        .unwrap();
    let mut lines = String::new();
    wait_for(&mut watch, "ready", &mut lines);

    // Ensure again: nothing to do, and nothing done.
    let d = sandbox::resolve(
        &client,
        &spec,
        &Default::default(),
        std::path::Path::new("/"),
    )
    .unwrap();
    let plan = sandbox::plan_desired(&client, &d, DiffOptions::default()).unwrap();
    assert!(plan.is_noop(), "{:?}", plan.actions);
    let report = sandbox::ensure(&client, &d, EnsureOptions::default(), &mut |l| {
        eprintln!("{l}")
    })
    .unwrap();
    assert!(!report.created);
    assert!(
        report.applied.iter().all(|a| !a.is_change()),
        "{:?}",
        report.applied
    );

    // The watch still sees host-side changes: the mount was not touched.
    std::fs::write(web.path().join("after-ensure"), "x").unwrap();
    wait_for(&mut watch, "event after-ensure", &mut lines);
    std::fs::write(web.path().join("stop"), "x").unwrap();
    assert_eq!(watch.wait().unwrap(), 0);

    // Drift: a changed limit is patched, the device still left alone.
    sb.exec(["true"]).unwrap();
    let spec2 = spec.clone().cpus(3);
    let d2 = sandbox::resolve(
        &client,
        &spec2,
        &Default::default(),
        std::path::Path::new("/"),
    )
    .unwrap();
    let plan = sandbox::plan_desired(&client, &d2, DiffOptions::default()).unwrap();
    assert_eq!(plan.actions.len(), 1, "{:?}", plan.actions);
    sandbox::ensure(&client, &d2, EnsureOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(sb.info().unwrap().config["limits.cpu"], "3");
    assert!(
        sandbox::plan_desired(&client, &d2, DiffOptions::default())
            .unwrap()
            .is_noop()
    );

    // A stopped sandbox is started by ensure.
    sb.stop(true, Duration::from_secs(5)).unwrap();
    sandbox::ensure(&client, &d2, EnsureOptions::default(), &mut |_| {}).unwrap();
    assert!(sb.info().unwrap().status.eq_ignore_ascii_case("running"));
}

/// Streaming, exit codes, argv fidelity, users, tty and stdin handling.
#[test]
fn exec_semantics() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let name = test_name("exec");
    cleanup.instance(&name);
    let sb = Sandbox::create(&client, &base_spec(&name)).expect("create");

    // Exit codes propagate, stdout and stderr stay separate.
    let out = sb
        .exec(["sh", "-c", "echo out; echo err >&2; exit 7"])
        .unwrap();
    assert_eq!(out.exit_code, 7);
    assert_eq!(out.stdout_text(), "out\n");
    assert_eq!(out.stderr_text(), "err\n");

    // argv is never joined into a shell string.
    let out = sb
        .exec(["printf", "[%s]", "a b", "$HOME", "\"q\"", ";id"])
        .unwrap();
    assert_eq!(out.stdout_text(), "[a b][$HOME][\"q\"][;id]");

    // Output is streamed as produced, not buffered until exit.
    let started = Instant::now();
    let mut s = sb
        .exec_stream(
            ["sh", "-c", "echo first; sleep 3; echo second"],
            ExecOptions::default(),
        )
        .unwrap();
    match s.next_event() {
        Some(ExecEvent::Stdout(b)) => assert_eq!(b, b"first\n"),
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "first chunk took {:?}",
        started.elapsed()
    );
    assert_eq!(s.wait().unwrap(), 0);
    assert!(started.elapsed() >= Duration::from_secs(3));

    // Users, cwd, env.
    let out = sb
        .exec_with(
            ["sh", "-c", "id -u; pwd; echo $HOME $FOO"],
            ExecOptions::default()
                .user("dev")
                .cwd("/tmp")
                .env("FOO", "bar"),
        )
        .unwrap();
    assert_eq!(out.stdout_text(), "1000\n/tmp\n/home/dev bar\n");
    let out = sb
        .exec_with(
            ["sh", "-c", "echo $0"],
            ExecOptions::default().user("dev").login(true),
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr_text());

    // tty vs no tty.
    let out = sb.exec(["tty"]).unwrap();
    assert_ne!(out.exit_code, 0);
    let out = sb
        .exec_with(
            ["sh", "-c", "tty; stty size"],
            ExecOptions::default().tty(true),
        )
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(
        out.stdout_text().contains("/dev/pts/"),
        "{:?}",
        out.stdout_text()
    );
    assert!(
        out.stdout_text().contains("24 80"),
        "{:?}",
        out.stdout_text()
    );

    // stdin: closed by default (cat returns at once), bytes, piped.
    let t = Instant::now();
    let out = sb.exec(["cat"]).unwrap();
    assert_eq!((out.exit_code, out.stdout.len()), (0, 0));
    assert!(t.elapsed() < Duration::from_secs(5));
    let out = sb
        .exec_with(
            ["wc", "-c"],
            ExecOptions::default().stdin(Stdin::Bytes(vec![b'x'; 200_000])),
        )
        .unwrap();
    assert_eq!(out.stdout_text().trim(), "200000");
    let mut s = sb
        .exec_stream(["cat"], ExecOptions::default().stdin(Stdin::Piped))
        .unwrap();
    s.write_stdin(b"one ").unwrap();
    s.write_stdin(b"two").unwrap();
    s.close_stdin();
    let out = s.collect_output().unwrap();
    assert_eq!(out.stdout_text(), "one two");

    // Signals and timeouts.
    let s = sb
        .exec_stream(
            ["sh", "-c", "trap 'exit 42' TERM; sleep 30 & wait"],
            ExecOptions::default(),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    s.signal(15).unwrap();
    assert_eq!(s.wait().unwrap(), 42);
    let t = Instant::now();
    let r = sb.exec_with(
        ["sleep", "30"],
        ExecOptions::default().timeout(Duration::from_secs(1)),
    );
    assert!(matches!(r, Err(Error::ExecTimeout { .. })), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(15));

    // No default timeout: a command quiet for longer than the socket timeout is fine.
    let out = sb.exec(["sh", "-c", "sleep 35; echo done"]).unwrap();
    assert_eq!(out.stdout_text(), "done\n");
}

/// The CLI: exit codes, a never-EOF inherited stdin, forwarded stdin, SIGINT.
#[test]
fn cli_exec() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let name = test_name("cli");
    cleanup.instance(&name);
    Sandbox::create(&client, &base_spec(&name)).expect("create");
    let bin = isb_bin();

    let st = Command::new(&bin)
        .args(["exec", &name, "--", "sh", "-c", "exit 3"])
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(3));

    // A stdin pipe that never reaches EOF (the parent keeps it open and never
    // writes) must not hang a command that does not read it.
    let t = Instant::now();
    let mut child = Command::new(&bin)
        .args(["exec", "-T", &name, "--", "echo", "fine"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _keep_stdin_open = child.stdin.take();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let st = child.wait().unwrap();
    assert_eq!((st.code(), out.as_str()), (Some(0), "fine\n"));
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "took {:?}",
        t.elapsed()
    );

    // Forwarded stdin reaches the command, and EOF closes it.
    let mut child = Command::new(&bin)
        .args(["exec", &name, "--", "tr", "a-z", "A-Z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(out, "HELLO");
    assert!(child.wait().unwrap().success());

    // SIGINT to isb reaches the command.
    let mut child = Command::new(&bin)
        .args(["exec", &name, "--", "sleep", "60"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&child),
        rustix::process::Signal::INT,
    )
    .unwrap();
    let t = Instant::now();
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(130));
    assert!(t.elapsed() < Duration::from_secs(10));

    // A tty, when stdin and stdout are terminals (via script(1), if present).
    if Command::new("script").arg("--version").output().is_ok() {
        let out = Command::new("script")
            .args([
                "-qec",
                &format!("{} exec {name} -- sh -c 'tty; exit 4'", bin.display()),
                "/dev/null",
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("/dev/pts/"));
        assert_eq!(out.status.code(), Some(4));
    }
}

/// A named volume with an owner, and both proxy directions.
#[test]
fn named_volume_owner_and_proxies() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let name = test_name("vol");
    let vol = test_name("vol");
    cleanup.instance(&name);
    let pool = sandbox::host_facts(&client)
        .unwrap()
        .pick_pool(None)
        .unwrap();
    cleanup.volume(&pool, &vol);

    // Host service the guest will reach through a bind=guest proxy.
    let host_srv = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = host_srv.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in host_srv.incoming().take(5) {
            let mut s = s.unwrap();
            eprintln!("host service: connection from {:?}", s.peer_addr());
            let _ = s.write_all(b"hello from host\n");
            let _ = s.shutdown(std::net::Shutdown::Write);
            let mut sink = Vec::new();
            let _ = s.read_to_end(&mut sink);
        }
    });
    let publish = free_port();

    let spec = base_spec(&name)
        .volume(
            "/home/dev/.cache/isbtest/data",
            Volume::named(&vol).owner("dev").device("data"),
        )
        .port(
            PortBinding::guest("tcp:127.0.0.1:9000", format!("tcp:127.0.0.1:{host_port}"))
                .name("backend"),
        )
        .port(
            PortBinding::host(format!("tcp:127.0.0.1:{publish}"), "tcp:127.0.0.1:8000").name("web"),
        )
        .ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]);
    let sb = Sandbox::connect_or_create(&client, &spec).expect("create");
    assert!(isb::volume::get(&client, &pool, &vol).unwrap().is_some());

    // The mount point and the parents it conjured belong to dev; home did not change.
    let out = sb
        .exec([
            "stat",
            "-c",
            "%U %n",
            "/home/dev/.cache/isbtest/data",
            "/home/dev/.cache/isbtest",
            "/home/dev",
        ])
        .unwrap();
    assert_eq!(
        out.stdout_text(),
        "dev /home/dev/.cache/isbtest/data\ndev /home/dev/.cache/isbtest\ndev /home/dev\n"
    );
    let out = sb
        .exec_with(
            ["touch", "/home/dev/.cache/isbtest/data/x"],
            ExecOptions::default().user("dev"),
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr_text());

    // bind=guest: guest 127.0.0.1:9000 -> host.
    let out = sb
        .exec(["python3", "-c", "import socket; s=socket.create_connection(('127.0.0.1', 9000), 5); print(s.recv(100).decode(), end='')"])
        .unwrap();
    assert_eq!(
        out.stdout_text(),
        "hello from host\n",
        "stderr: {}",
        out.stderr_text()
    );

    // bind=host: host 127.0.0.1:<publish> -> guest 8000.
    let mut srv = sb
        .exec_stream(
            ["python3", "-c", "import socket; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', 8000)); s.listen(1); print('listening', flush=True); c,_=s.accept(); c.sendall(b'hello from guest\\n'); c.close()"],
            ExecOptions::default(),
        )
        .unwrap();
    assert!(matches!(srv.next_event(), Some(ExecEvent::Stdout(b)) if b.starts_with(b"listening")));
    let mut conn = std::net::TcpStream::connect(("127.0.0.1", publish)).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut got = String::new();
    BufReader::new(&mut conn).read_line(&mut got).unwrap();
    assert_eq!(got, "hello from guest\n");
    assert_eq!(srv.wait().unwrap(), 0);

    // Port search steps past a taken host port, and a re-add is a no-op.
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = taken.local_addr().unwrap().port();
    let listen = sb
        .add_port(
            &PortBinding::host(format!("tcp:127.0.0.1:{base}"), "tcp:127.0.0.1:8001")
                .name("searched")
                .search(20),
        )
        .unwrap();
    assert_ne!(listen, format!("tcp:127.0.0.1:{base}"));
    let again = sb
        .add_port(
            &PortBinding::host(format!("tcp:127.0.0.1:{base}"), "tcp:127.0.0.1:8001")
                .name("searched")
                .search(20),
        )
        .unwrap();
    assert_eq!(listen, again);
    assert!(sb.remove_device("searched").unwrap());
    assert!(!sb.remove_device("searched").unwrap());

    // The volume outlives the sandbox and is refused for deletion while used.
    assert!(isb::volume::remove(&client, &pool, &vol).is_err());
}

/// Labels, list filters, and prune (dry run, then -y), on isb-test-* only.
#[test]
fn labels_and_prune() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let keep_dir = tempdir();
    let gone_dir = tempdir();
    let label = format!("isb-test.path-{}", std::process::id());
    let keep = test_name("keep");
    let gone = test_name("gone");
    cleanup.instance(&keep);
    cleanup.instance(&gone);
    Sandbox::create(
        &client,
        &base_spec(&keep).label(&label, keep_dir.path().to_str().unwrap()),
    )
    .unwrap();
    Sandbox::create(
        &client,
        &base_spec(&gone).label(&label, gone_dir.path().to_str().unwrap()),
    )
    .unwrap();

    let by_key = Sandbox::list_with(&client, &[LabelFilter::parse(&label)]).unwrap();
    let mut names: Vec<&str> = by_key.iter().map(|i| i.name.as_str()).collect();
    names.sort();
    let mut want = vec![gone.as_str(), keep.as_str()];
    want.sort();
    assert_eq!(names, want);
    let by_val = Sandbox::list_with(
        &client,
        &[LabelFilter::parse(&format!(
            "{label}={}",
            keep_dir.path().display()
        ))],
    )
    .unwrap();
    assert_eq!(by_val.len(), 1);
    assert_eq!(by_val[0].name, keep);

    let gone_path = gone_dir.path().to_path_buf();
    drop(gone_dir);
    assert!(!gone_path.exists());

    // Through the CLI: dry run by default.
    let bin = isb_bin();
    let out = Command::new(&bin)
        .args(["prune", "--label", &label, "--missing-path", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let items: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], gone.as_str());
    assert_eq!(items[0]["deleted"], false);
    assert!(Sandbox::get(&client, &gone).is_ok());

    let out = Command::new(&bin)
        .args(["prune", "--label", &label, "--missing-path", "-y"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(Sandbox::get(&client, &gone).is_err());
    assert!(Sandbox::get(&client, &keep).is_ok());

    // `ls --label` through the CLI.
    let out = Command::new(&bin)
        .args(["ls", "--label", &label, "--json"])
        .output()
        .unwrap();
    let items: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["labels"]["isb-test"], "1");
}

/// The stuck-operation paths: a wait deadline names the step, a create that
/// outlives its deadline is reported and kept once it settles, and cleanup never
/// deletes an instance this call did not create.
#[test]
fn stuck_operation_paths() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);

    // Deadline on a running operation.
    let name = test_name("stuck");
    cleanup.instance(&name);
    let sb = Sandbox::create(&client, &base_spec(&name)).unwrap();
    let op = client
        .start_operation(
            "POST",
            &format!("/1.0/instances/{name}/exec"),
            Some(&serde_json::json!({"command": ["sleep", "5"], "wait-for-websocket": false, "record-output": false, "interactive": false})),
        )
        .unwrap();
    let t = Instant::now();
    match client.wait_operation(&op, "probe step", Duration::from_millis(1500)) {
        Err(Error::OperationTimeout {
            step, cancelled, ..
        }) => {
            assert_eq!(step, "probe step");
            assert!(!cancelled, "exec operations are not cancellable");
        }
        other => panic!("{other:?}"),
    }
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
    client
        .wait_operation(&op, "probe", Duration::from_secs(20))
        .unwrap();

    // A create past its deadline: reported as stalled, then kept once it settles.
    let late = test_name("late");
    cleanup.instance(&late);
    let slow = Client::new().timeouts(Timeouts {
        create: Duration::ZERO,
        settle: Duration::from_secs(120),
        ..Timeouts::default()
    });
    let mut lines = Vec::new();
    Sandbox::create_with(
        &slow,
        &base_spec(&late),
        &Default::default(),
        EnsureOptions::default(),
        &mut |l| lines.push(l.to_string()),
    )
    .unwrap();
    let all = lines.join("\n");
    assert!(
        all.contains(&format!("create instance {late} stalled")),
        "{all}"
    );
    assert!(all.contains("finished late"), "{all}");
    assert!(sandbox::cleanup_half_created(&client, &late, "not-our-token", &mut |_| {}).is_err());

    // Cleanup refuses an instance with someone else's token (or none)...
    let r = sandbox::cleanup_half_created(&client, &name, "another-callers-token", &mut |_| {});
    assert!(matches!(r, Err(Error::AlreadyExists(_))), "{r:?}");
    assert!(sb.info().is_ok());
    // ...and deletes one carrying its own.
    let token = sb.info().unwrap().config[sandbox::CREATE_TOKEN_KEY].clone();
    sandbox::cleanup_half_created(&client, &name, &token, &mut |_| {}).unwrap();
    assert!(Sandbox::get(&client, &name).is_err());
}

/// Compose: up, plan, exec a service, down, through the CLI.
#[test]
fn compose_cli() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let dir = tempdir();
    let web = dir.path().join("web");
    std::fs::create_dir(&web).unwrap();
    let name = test_name("compose");
    // Named volumes are `<project>_<key>`, as in docker compose.
    let proj = test_name("cproj");
    let vol = format!("{proj}_data");
    cleanup.instance(&name);
    let pool = sandbox::host_facts(&client)
        .unwrap()
        .pick_pool(None)
        .unwrap();
    cleanup.volume(&pool, &vol);
    let file = dir.path().join("isb.yaml");
    std::fs::write(
        &file,
        format!(
            r#"
name: {proj}
x-limits: &limits
  cpus: 2
  mem_limit: 1g
volumes:
  data: {{}}
services:
  web:
    <<: *limits
    container_name: "${{ISB_T_NAME}}"
    image: "${{ISB_T_IMAGE:-dev-base}}"
    idmap: auto
    labels: [isb-test=1, "isb-test.web=${{ISB_T_WEB}}"]
    volumes:
      - {{ type: bind, source: ./web, target: /home/dev/web, device: web }}
      - {{ source: data, target: /home/dev/.cache/t, owner: dev }}
    ports:
      - {{ name: http, published: "${{ISB_T_PORT}}-${{ISB_T_PORT_END}}", target: 8000 }}
    ready: [running, default_route, {{user_exists: dev}}, {{path_writable: /home/dev/web}}]
    user: dev
    working_dir: /home/dev/web
    exec:
      env: [GREETING=hello]
"#
        ),
    )
    .unwrap();
    let bin = isb_bin();
    let port = free_port().to_string();
    let port_end = (port.parse::<u16>().unwrap() + 20).to_string();
    let run = |args: &[&str]| {
        let out = Command::new(&bin)
            .arg("-f")
            .arg(&file)
            .args(args)
            .env("ISB_T_NAME", &name)
            .env("ISB_T_IMAGE", image())
            .env("ISB_T_WEB", &web)
            .env("ISB_T_PORT", &port)
            .env("ISB_T_PORT_END", &port_end)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, out, err) = run(&["plan", "--exit-code"]);
    assert_eq!(code, Some(2), "{out}{err}");
    assert!(out.contains("+ create"), "{out}");
    // `up` prints the settled address of a searched port, both when it adds
    // the device and when it finds it already in place.
    let want = format!("web http tcp:127.0.0.1:{port}\n");
    let (code, out, err) = run(&["up", "-d"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, want);
    let (code, out, err) = run(&["up", "-d"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, want);
    let (code, out, _) = run(&["port", "get", &name, "http"]);
    assert_eq!((code, out), (Some(0), format!("tcp:127.0.0.1:{port}\n")));
    let (code, out, _) = run(&["port", "get", &name, "http", "connect"]);
    assert_eq!((code, out.as_str()), (Some(0), "tcp:127.0.0.1:8000\n"));
    let (code, _, _) = run(&["port", "get", &name, "nope"]);
    assert_eq!(code, Some(1));
    let (code, out, _) = run(&["port", "ls", &name, "--json"]);
    assert_eq!(code, Some(0));
    let ports: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(ports["http"]["listen"], format!("tcp:127.0.0.1:{port}"));
    let (code, out, err) = run(&["plan", "--exit-code"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("up to date"), "{out}");
    let (code, out, _) = run(&["plan", "--json"]);
    assert_eq!(code, Some(0));
    let plans: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(plans[0]["name"], name.as_str());

    // exec by service name uses the service's exec defaults.
    let (code, out, err) = run(&[
        "exec",
        "web",
        "--",
        "sh",
        "-c",
        "id -un; pwd; echo $GREETING > f; echo $GREETING",
    ]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "dev\n/home/dev/web\nhello\n");
    assert_eq!(std::fs::read_to_string(web.join("f")).unwrap(), "hello\n");

    let (code, out, _) = run(&["ps", "--json"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("\"service\": \"web\""), "{out}");
    let (code, out, _) = run(&["config"]);
    assert_eq!(code, Some(0));
    assert!(out.contains(&name), "{out}");

    let (code, _, err) = run(&["down", "--volumes"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(Sandbox::get(&client, &name).is_err());
    assert!(isb::volume::get(&client, &pool, &vol).unwrap().is_none());
}

/// Foreground `up`: the command's output is streamed and its exit code passed
/// through, and the sandbox is stopped when the command exits or when the
/// process that started isb dies without signalling it.
#[test]
fn compose_foreground() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let name = test_name("fg");
    let mut cleanup = Cleanup::new(&client);
    cleanup.instance(&name);
    let dir = tempdir();
    let file = dir.path().join("isb.yaml");
    let write = |cmd: &str| {
        std::fs::write(
            &file,
            format!(
                "services:\n  web:\n    container_name: {name}\n    image: {}\n    command: [sh, -c, {cmd:?}]\n",
                image()
            ),
        )
        .unwrap()
    };
    let status = |c: &Client| Sandbox::get(c, &name).unwrap().info().unwrap().status;

    write("echo hi; exit 3");
    let out = Command::new(isb_bin())
        .args(["-q", "-f"])
        .arg(&file)
        .arg("up")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "web | hi\n");
    assert_eq!(status(&client), "Stopped");

    // The parent is SIGKILLed, so isb gets no signal: only the watchdog can
    // notice.
    write("echo hi; sleep 600");
    let log = dir.path().join("up.log");
    let mut parent = Command::new("sh")
        .arg("-c")
        .arg(r#""$1" -f "$2" up >"$3" 2>&1 & exec sleep 600"#)
        .arg("sh")
        .arg(isb_bin())
        .arg(&file)
        .arg(&log)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while !std::fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("web | hi")
    {
        assert!(Instant::now() < deadline, "command never ran");
        std::thread::sleep(Duration::from_millis(200));
    }
    parent.kill().unwrap();
    parent.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while status(&client) != "Stopped" {
        assert!(Instant::now() < deadline, "sandbox still running");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A virtual machine: the other first-class instance type. Boots, waits for the
/// agent, execs, and shares a host path over virtiofs.
#[test]
fn virtual_machine() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let vm_image = std::env::var("ISB_TEST_VM_IMAGE").unwrap_or_else(|_| "deb13-cloud-vm".into());
    if client
        .server_info()
        .map(|i| !i.to_string().contains("qemu"))
        .unwrap_or(true)
    {
        eprintln!("skipped: incusd has no qemu driver");
        return;
    }
    let mut cleanup = Cleanup::new(&client);
    let name = test_name("vm");
    cleanup.instance(&name);
    let share = tempdir();
    std::fs::write(share.path().join("from-host"), "hello vm\n").unwrap();
    let mut spec = SandboxSpec::new(&name, &vm_image)
        .cpus(2)
        .memory("1GiB")
        .label("isb-test", "1")
        .volume(
            "/mnt/share",
            Volume::bind(share.path().to_str().unwrap()).device("share"),
        )
        .ready_timeout("240s");
    spec.instance_type = isb::InstanceType::VirtualMachine;

    let t = Instant::now();
    let sb = match Sandbox::create(&client, &spec) {
        Err(Error::Invalid(m)) if m.contains("not found locally") => {
            eprintln!("skipped: no local VM image {vm_image}");
            return;
        }
        Err(e) => {
            // Evidence for a VM that did not come up: its console and state.
            for args in [vec!["console", &name, "--show-log"], vec!["info", &name]] {
                if let Ok(o) = Command::new("incus").args(&args).output() {
                    let t = String::from_utf8_lossy(&o.stdout);
                    let tail: Vec<&str> = t.lines().rev().take(40).collect();
                    eprintln!(
                        "--- incus {}:\n{}",
                        args[0],
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                    );
                }
            }
            panic!("create vm: {e:?}");
        }
        Ok(sb) => sb,
    };
    eprintln!("vm ready in {:?}", t.elapsed());
    let info = sb.info().unwrap();
    assert_eq!(info.instance_type, "virtual-machine");

    // Its own kernel, exec over the agent, exit codes.
    let out = sb
        .exec(["sh", "-c", "cat /mnt/share/from-host; exit 3"])
        .unwrap();
    assert_eq!(out.exit_code, 3);
    assert_eq!(out.stdout_text(), "hello vm\n", "{}", out.stderr_text());
    let out = sb
        .exec(["sh", "-c", "echo back > /mnt/share/from-vm"])
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr_text());
    assert_eq!(
        std::fs::read_to_string(share.path().join("from-vm")).unwrap(),
        "back\n"
    );
    let out = sb
        .exec_with(["tty"], ExecOptions::default().tty(true))
        .unwrap();
    assert!(
        out.stdout_text().contains("/dev/pts/"),
        "{:?}",
        out.stdout_text()
    );

    // A no-op ensure on a VM changes nothing either.
    let d = sandbox::resolve(
        &client,
        &spec,
        &Default::default(),
        std::path::Path::new("/"),
    )
    .unwrap();
    let plan = sandbox::plan_desired(&client, &d, DiffOptions::default()).unwrap();
    assert!(plan.is_noop(), "{:?}", plan.actions);
}

fn http_get(addr: &str) -> std::io::Result<String> {
    let mut s = std::net::TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(b"GET / HTTP/1.0\r\n\r\n")?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string())
}

/// `restart:` supervises the command in the guest: it is restarted by
/// systemd when killed, and after a reboot it has its secret back with no
/// isb running.
#[test]
fn long_running_service() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let mut cleanup = Cleanup::new(&client);
    let dir = tempdir();
    let name = test_name("svc");
    cleanup.instance(&name);
    let yaml = format!(
        "secrets: {{tok: {{environment: ISB_TEST_TOKEN}}}}\n\
         services:\n  app:\n    container_name: {name}\n    image: {}\n    labels: {{isb-test: '1'}}\n\
         \x20   restart: always\n    user: dev\n    secrets: [{{source: tok, uid: 1000}}]\n\
         \x20   command: [python3, -m, http.server, '8000', -d, /run/secrets]\n",
        image()
    );
    let lookup = |k: &str| (k == "ISB_TEST_TOKEN").then(|| "t0ken".to_string());
    let mut p = isb::compose::load_docs(
        &[(dir.path().join("isb.yaml"), yaml)],
        dir.path(),
        Some("isbtest"),
        &lookup,
    )
    .unwrap();
    p.vars.insert("ISB_TEST_TOKEN".into(), "t0ken".into());
    let ups = isb::compose::up_handles(&client, &p, &[], EnsureOptions::default(), &mut |l| {
        eprintln!("{l}")
    })
    .unwrap();
    let sb = &ups[0].2;
    let fetch = |sb: &Sandbox| {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let o = sb
                .exec_with(
                    ["python3", "-c", "import urllib.request as u; print(u.urlopen('http://127.0.0.1:8000/tok').read().decode())"],
                    ExecOptions::default(),
                )
                .unwrap();
            if o.success() {
                return o.stdout_text().trim().to_string();
            }
            assert!(
                Instant::now() < deadline,
                "app never served: {}",
                o.stderr_text()
            );
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    assert_eq!(fetch(sb), "t0ken");
    assert_eq!(isb::supervise::unit_state(sb, "app").unwrap(), "active");
    // Killed: systemd brings it back.
    sb.exec(["pkill", "-f", "http.server"]).unwrap();
    assert_eq!(fetch(sb), "t0ken");
    // Rebooted: /run/secrets is a fresh tmpfs, restored by the unit itself.
    sb.restart().unwrap();
    assert_eq!(fetch(sb), "t0ken");
    let info = sb.info().unwrap();
    assert_eq!(
        info.config.get("boot.autostart").map(String::as_str),
        Some("true")
    );
}

/// A secret store under `state` with a throwaway key.
fn test_secrets(state: &std::path::Path) -> std::sync::Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        std::sync::Arc::new(k),
    )))
}

/// The stack controller: replicas behind the balancer, a forced rolling
/// redeploy with no failed request, scale down, remove.
#[test]
fn stack_controller() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let secrets = test_secrets(state.path());
    let ctl = isb::stack::Controller::start(client.clone(), store, Duration::from_secs(2), secrets)
        .unwrap();
    let stack = format!("isb-test-{}", std::process::id() % 100000);
    let port = free_port();
    let yaml = format!(
        "services:\n  web:\n    image: {}\n    labels: {{isb-test: '1'}}\n    user: dev\n\
         \x20   command: [sh, -c, 'hostname > /tmp/index.html && exec python3 -m http.server 8000 -d /tmp']\n\
         \x20   ports: ['127.0.0.1:{port}:8000']\n\
         \x20   healthcheck: {{test: [CMD, python3, -c, \"import urllib.request as u; u.urlopen('http://127.0.0.1:8000')\"], interval: 2s, start_interval: 1s}}\n\
         \x20   deploy: {{replicas: 2, update_config: {{order: start-first, monitor: 2s}}}}\n",
        image()
    );
    let p = isb::compose::load_docs(
        &[(state.path().join("isb.yaml"), yaml)],
        state.path(),
        Some(&stack),
        &|_| None,
    )
    .unwrap();
    let def = isb::stack::StackDef {
        name: stack.clone(),
        org: isb::org::OrgId::default_org(),
        file: p.file,
        base_dir: state.path().to_path_buf(),
        secrets: Default::default(),
        force: Default::default(),
        deployed_at: 0,
        deployed_by: "test".into(),
        previous: None,
    };
    struct Rm(isb::stack::Controller, String);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
        }
    }
    let _rm = Rm(ctl.clone(), stack.clone());
    ctl.deploy(def).unwrap();
    let st = isb::daemon::wait_settled(&ctl, &stack, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    let addr = format!("127.0.0.1:{port}");
    let seen: std::collections::BTreeSet<String> =
        (0..8).map(|_| http_get(&addr).unwrap()).collect();
    assert_eq!(seen.len(), 2, "{seen:?}");

    // A forced redeploy replaces both replicas while requests keep landing.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let load = {
        let (stop, addr) = (stop.clone(), addr.clone());
        std::thread::spawn(move || {
            let (mut ok, mut failed) = (0, 0);
            while !stop.load(Ordering::SeqCst) {
                match http_get(&addr) {
                    Ok(b) if !b.is_empty() => ok += 1,
                    _ => failed += 1,
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            (ok, failed)
        })
    };
    ctl.redeploy(&stack, "web").unwrap();
    let st = isb::daemon::wait_settled(&ctl, &stack, Duration::from_secs(300)).unwrap();
    stop.store(true, Ordering::SeqCst);
    let (ok, failed) = load.join().unwrap();
    assert!(st.converged, "{st:?}");
    let after: std::collections::BTreeSet<String> =
        (0..8).map(|_| http_get(&addr).unwrap()).collect();
    assert!(after.is_disjoint(&seen), "{after:?} vs {seen:?}");
    assert!(ok > 0 && failed == 0, "ok {ok}, failed {failed}");

    ctl.scale(&stack, "web", 1).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while ctl.status(&stack).unwrap().services[0].instances.len() != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_secs(1));
    }
    ctl.remove(&stack, true, Duration::from_secs(120)).unwrap();
    let left = Sandbox::list_with(&client, &[LabelFilter::parse("isb-test")])
        .unwrap()
        .into_iter()
        .filter(|i| i.name.starts_with(&stack))
        .count();
    assert_eq!(left, 0);
}

/// Two orgs: each sees only its own instances, members of one org reach
/// each other by name, and nothing in one org reaches the other.
/// Needs `isb host setup` on a host with a default-deny firewall.
#[test]
fn orgs_isolate() {
    if !enabled() {
        return;
    }
    let base = Client::new();
    let a = isb::org::OrgId::new(format!("isbtest-a{}", std::process::id() % 100000)).unwrap();
    let b = isb::org::OrgId::new(format!("isbtest-b{}", std::process::id() % 100000)).unwrap();
    struct Rm(Client, Vec<isb::org::OrgId>);
    impl Drop for Rm {
        fn drop(&mut self) {
            for o in &self.1 {
                let _ = isb::org::remove(&self.0, o, true, &mut |_| {});
            }
        }
    }
    let _rm = Rm(base.clone(), vec![a.clone(), b.clone()]);
    let opts = isb::org::OrgOptions {
        cpus: Some(4),
        memory: Some("4GiB".into()),
        ..Default::default()
    };
    isb::org::ensure(&base, &a, &opts, &mut |l| eprintln!("{l}")).unwrap();
    isb::org::ensure(&base, &b, &opts, &mut |l| eprintln!("{l}")).unwrap();
    let (ca, cb) = (isb::org::client(&base, &a), isb::org::client(&base, &b));
    let mk = |c: &Client, n: &str| {
        let spec =
            SandboxSpec::new(n, image()).ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]);
        Sandbox::create(c, &spec).unwrap()
    };
    let web = mk(&ca, "web");
    let _db = mk(&ca, "db");
    let other = mk(&cb, "other");
    // Visibility: org b cannot see org a's instances.
    assert!(Sandbox::get(&cb, "web").is_err());
    assert_eq!(Sandbox::list(&ca).unwrap().len(), 2);
    let ip = |sb: &Sandbox| {
        let o = sb
            .exec([
                "sh",
                "-c",
                "ip -4 -o addr show eth0 | awk '{print $4}' | cut -d/ -f1",
            ])
            .unwrap();
        o.stdout_text().trim().to_string()
    };
    let other_ip = ip(&other);
    let ping =
        |sb: &Sandbox, target: &str| sb.exec(["ping", "-c1", "-W2", target]).unwrap().success();
    assert!(ping(&web, &format!("db.{a}.isb")), "same-org name");
    assert!(!ping(&web, &other_ip), "cross-org reachable");
    assert!(!ping(&other, &ip(&web)), "cross-org reachable (b to a)");
    // A restricted org refuses what would reach the host.
    let bad = SandboxSpec::new("bad", image()).privileged(true);
    assert!(Sandbox::create(&ca, &bad).is_err());
    let bind = SandboxSpec::new("bind", image()).volume("/hostetc", Volume::bind("/etc"));
    assert!(Sandbox::create(&ca, &bind).is_err());
}

/// Removes test orgs (and everything in them) when dropped.
struct OrgsRm(Client, Vec<isb::org::OrgId>);

impl Drop for OrgsRm {
    fn drop(&mut self) {
        for o in &self.1 {
            assert!(o.as_str().starts_with("isbtest-"));
            if let Err(e) = isb::org::remove(&self.0, o, true, &mut |_| {}) {
                if !e.is_not_found() {
                    eprintln!("cleanup: org {o}: {e}");
                }
            }
        }
    }
}

/// What a name resolves to inside an instance (IPv4, deduplicated).
fn resolve(sb: &Sandbox, name: &str) -> std::collections::BTreeSet<String> {
    let o = sb.exec(["getent", "ahostsv4", name]).unwrap();
    o.stdout_text()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect()
}

/// Wait until `name` resolves to exactly `want` inside `sb`.
fn wait_resolves(sb: &Sandbox, name: &str, want: &std::collections::BTreeSet<String>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let got = resolve(sb, name);
        if got == *want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} resolves to {got:?}, want {want:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Service discovery: `<service>.<stack>.<org>.isb` (and `<service>.<stack>`)
/// resolves to every in-rotation replica, follows a rolling replacement, and
/// goes away with the stack. Needs `isb host setup` (the hosts directory).
#[test]
fn service_names() {
    if !enabled() {
        return;
    }
    let base = Client::new();
    let org = isb::org::OrgId::new(format!("isbtest-d{}", std::process::id() % 100000)).unwrap();
    let _rm = OrgsRm(base.clone(), vec![org.clone()]);
    let info = isb::org::ensure(
        &base,
        &org,
        &isb::org::OrgOptions {
            cpus: Some(8),
            memory: Some("8GiB".into()),
            ..Default::default()
        },
        &mut |l| eprintln!("{l}"),
    )
    .unwrap();
    assert!(
        info.dns_dir.is_some(),
        "no service names: run `sudo isb host setup` first"
    );
    let oc = isb::org::client(&base, &org);
    let client = Sandbox::create(
        &oc,
        &SandboxSpec::new("client", image())
            .ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let store = isb::stack::Store::open(state.path()).unwrap();
    let ctl = isb::stack::Controller::start(
        base.clone(),
        store,
        Duration::from_secs(2),
        test_secrets(state.path()),
    )
    .unwrap();
    let stack = format!("isb-test-{}", std::process::id() % 100000);
    let yaml = format!(
        "services:\n  web:\n    image: {}\n    user: dev\n\
         \x20   command: [sh, -c, 'exec python3 -m http.server 8000 -d /tmp']\n\
         \x20   healthcheck: {{test: [CMD, python3, -c, \"import urllib.request as u; u.urlopen('http://127.0.0.1:8000')\"], interval: 2s, start_interval: 1s}}\n\
         \x20   deploy: {{replicas: 2, update_config: {{order: start-first, monitor: 2s}}}}\n",
        image()
    );
    let p = isb::compose::load_docs(
        &[(state.path().join("isb.yaml"), yaml)],
        state.path(),
        Some(&stack),
        &|_| None,
    )
    .unwrap();
    let def = isb::stack::StackDef {
        name: stack.clone(),
        org: org.clone(),
        file: p.file,
        base_dir: state.path().to_path_buf(),
        secrets: Default::default(),
        force: Default::default(),
        deployed_at: 0,
        deployed_by: "test".into(),
        previous: None,
    };
    let q = def.qualified();
    struct Rm(isb::stack::Controller, String);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = self.0.remove(&self.1, true, Duration::from_secs(120));
            self.0.shutdown();
        }
    }
    let _rmstack = Rm(ctl.clone(), q.clone());
    ctl.deploy(def).unwrap();
    let in_rotation = |ctl: &isb::stack::Controller| -> std::collections::BTreeSet<String> {
        ctl.status(&q).unwrap().services[0]
            .instances
            .iter()
            .filter(|i| i.in_rotation)
            .filter_map(|i| i.ip.clone())
            .collect()
    };
    let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    let first = in_rotation(&ctl);
    assert_eq!(first.len(), 2, "{st:?}");
    let full = format!("web.{stack}.{org}.isb");
    let short = format!("web.{stack}");
    wait_resolves(&client, &full, &first);
    wait_resolves(&client, &short, &first);

    // A rolling replacement: the name follows the new replicas.
    ctl.redeploy(&q, "web").unwrap();
    let st = isb::daemon::wait_settled(&ctl, &q, Duration::from_secs(300)).unwrap();
    assert!(st.converged, "{st:?}");
    let second = in_rotation(&ctl);
    assert_eq!(second.len(), 2, "{st:?}");
    assert!(second.is_disjoint(&first), "{second:?} vs {first:?}");
    wait_resolves(&client, &full, &second);

    // Gone with the stack.
    ctl.remove(&q, true, Duration::from_secs(120)).unwrap();
    wait_resolves(&client, &full, &Default::default());
}

/// An egress exception lets one org reach a private address the default
/// deny blocks, on the given port only, while another org stays blocked.
/// The target is a host address in a private range where sshd listens
/// (`ISB_TEST_EGRESS_IP`, default incusbr0's), reached from the org bridge
/// through the host's INPUT chain.
#[test]
fn egress_exceptions() {
    if !enabled() {
        return;
    }
    let target = std::env::var("ISB_TEST_EGRESS_IP").unwrap_or_else(|_| {
        let o = Command::new("ip")
            .args(["-4", "-o", "addr", "show", "incusbr0"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .nth(3)
            .and_then(|a| a.split('/').next())
            .expect("no incusbr0 address; set ISB_TEST_EGRESS_IP")
            .to_string()
    });
    let base = Client::new();
    let n = std::process::id() % 100000;
    let a = isb::org::OrgId::new(format!("isbtest-e{n}")).unwrap();
    let b = isb::org::OrgId::new(format!("isbtest-f{n}")).unwrap();
    let _rm = OrgsRm(base.clone(), vec![a.clone(), b.clone()]);
    let opts = |egress: Option<Vec<&str>>| isb::org::OrgOptions {
        cpus: Some(2),
        memory: Some("2GiB".into()),
        egress: egress.map(|v| {
            v.into_iter()
                .map(|e| isb::org::Egress::parse(e).unwrap())
                .collect()
        }),
        ..Default::default()
    };
    let port_only = format!("{target}:22/tcp");
    let info = isb::org::ensure(&base, &a, &opts(Some(vec![&port_only])), &mut |l| {
        eprintln!("{l}")
    })
    .unwrap();
    assert_eq!(info.egress, vec![format!("{target}/32:22/tcp")]);
    isb::org::ensure(&base, &b, &opts(None), &mut |l| eprintln!("{l}")).unwrap();
    let mk = |o: &isb::org::OrgId, n: &str| {
        let spec =
            SandboxSpec::new(n, image()).ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]);
        Sandbox::create(&isb::org::client(&base, o), &spec).unwrap()
    };
    let in_a = mk(&a, "probe");
    let in_b = mk(&b, "probe");
    let ssh = |sb: &Sandbox| {
        let o = sb
            .exec([
                "timeout",
                "5",
                "bash",
                "-c",
                &format!("exec 3<>/dev/tcp/{target}/22 && head -c 4 <&3"),
            ])
            .unwrap();
        o.stdout_text()
    };
    let ping = |sb: &Sandbox| sb.exec(["ping", "-c1", "-W2", &target]).unwrap().success();
    assert_eq!(
        ssh(&in_a),
        "SSH-",
        "the exception lets org a reach {target}:22"
    );
    assert_eq!(ssh(&in_b), "", "org b reached {target}:22");
    assert!(!ping(&in_a), "a port-limited exception let ICMP through");
    assert!(!ping(&in_b));

    // Replaced by a whole-address exception: everything to it passes.
    let info = isb::org::ensure(&base, &a, &opts(Some(vec![&target])), &mut |_| {}).unwrap();
    assert_eq!(info.egress, vec![format!("{target}/32")]);
    assert!(ping(&in_a), "a whole-address exception blocked ICMP");
    // Kept by an ensure that does not mention it.
    let info = isb::org::ensure(&base, &a, &opts(None), &mut |_| {}).unwrap();
    assert_eq!(info.egress, vec![format!("{target}/32")]);
    assert_eq!(ssh(&in_b), "");
}

/// Deploys a compose file as a stack on its own controller and store, the
/// way `stack_deploy` does: bind the secrets, then deploy. Removes the stack
/// when dropped.
struct SecretStack {
    ctl: isb::stack::Controller,
    secrets: std::sync::Arc<isb::secrets::Secrets>,
    name: String,
    state: tempfile::TempDir,
}

impl SecretStack {
    fn new(what: &str) -> SecretStack {
        let state = tempfile::tempdir().unwrap();
        let store = isb::stack::Store::open(state.path()).unwrap();
        let secrets = test_secrets(state.path());
        let ctl = isb::stack::Controller::start(
            Client::new(),
            store,
            Duration::from_secs(2),
            secrets.clone(),
        )
        .unwrap();
        SecretStack {
            ctl,
            secrets,
            name: format!("isb-test-{what}{}", std::process::id() % 100000),
            state,
        }
    }

    fn deploy(&self, yaml: &str, given: &[(&str, &str)]) {
        let dir = self.state.path();
        let p = isb::compose::load_docs(
            &[(dir.join("isb.yaml"), yaml.to_string())],
            dir,
            Some(&self.name),
            &|_| None,
        )
        .unwrap();
        let org = isb::org::OrgId::default_org();
        let given = given
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let secrets =
            isb::stack::secrets::bind(&self.secrets, &org, &self.name, &p.file, &given, false)
                .unwrap();
        let def = isb::stack::StackDef {
            name: self.name.clone(),
            org,
            file: p.file,
            base_dir: dir.to_path_buf(),
            secrets,
            force: Default::default(),
            deployed_at: 0,
            deployed_by: "test".into(),
            previous: None,
        };
        self.ctl.deploy(def).unwrap();
        self.settle();
    }

    fn settle(&self) -> isb::stack::controller::StackStatus {
        let st =
            isb::daemon::wait_settled(&self.ctl, &self.name, Duration::from_secs(300)).unwrap();
        assert!(st.converged, "{st:?}");
        st
    }

    /// The one instance of a service.
    fn instance(&self, service: &str) -> Sandbox {
        let st = self.ctl.status(&self.name).unwrap();
        let s = st.services.iter().find(|s| s.service == service).unwrap();
        assert_eq!(s.instances.len(), 1, "{s:?}");
        Sandbox::get(&Client::new(), &s.instances[0].name).unwrap()
    }
}

impl Drop for SecretStack {
    fn drop(&mut self) {
        let _ = self.ctl.remove(&self.name, true, Duration::from_secs(120));
        self.ctl.shutdown();
    }
}

fn read(sb: &Sandbox, path: &str) -> String {
    let o = sb.exec(["cat", path]).unwrap();
    assert!(o.success(), "cat {path}: {}", o.stderr_text());
    o.stdout_text().trim().to_string()
}

/// Wait until a file in the guest is non-empty.
fn wait_file(sb: &Sandbox, path: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !sb.exec(["test", "-s", path]).is_ok_and(|o| o.success()) {
        assert!(
            Instant::now() < deadline,
            "{}: {path} never written",
            sb.name()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A stack using an `external` secret: the value lands in /run/secrets, and
/// `isb secret set` (the store's set, then the controller told, as the
/// `secret_set` tool does) rolls the service to a new instance holding the
/// new value.
#[test]
fn stack_external_secret_rolls() {
    if !enabled() {
        return;
    }
    let s = SecretStack::new("ext");
    let org = isb::org::OrgId::default_org();
    let store_name = format!("{}.db", s.name);
    s.secrets
        .create(&org, &store_name, None, b"first", &Default::default())
        .unwrap();
    s.deploy(
        &format!(
            "secrets: {{db: {{external: true, name: {store_name}}}}}\n\
             services:\n  app:\n    image: {}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sleep, infinity]\n    secrets: [db]\n",
            image()
        ),
        &[],
    );
    let before = s.instance("app");
    assert_eq!(read(&before, "/run/secrets/db"), "first");
    // The definition holds the reference, never the value.
    let def = s.ctl.definition(&s.name).unwrap();
    assert_eq!(def.secrets["db"].name, store_name);
    assert_eq!(def.secrets["db"].version, 1);
    assert!(!serde_json::to_string(&def).unwrap().contains("first"));
    let rev = def.revision("app").unwrap();

    s.secrets.set(&org, &store_name, b"second").unwrap();
    assert_eq!(
        s.ctl.secret_changed(&org, &store_name),
        std::slice::from_ref(&s.name)
    );
    let st = s.settle();
    assert_ne!(st.services[0].rev, rev);
    let after = s.instance("app");
    assert_ne!(after.name(), before.name(), "a new instance");
    assert_eq!(read(&after, "/run/secrets/db"), "second");
    // An unchanged version rolls nothing.
    assert!(s.ctl.secret_changed(&org, &store_name).is_empty());
    // A reboot restores the file from the guest's own copy.
    after.restart().unwrap();
    wait_file(&after, "/run/secrets/db");
    assert_eq!(read(&after, "/run/secrets/db"), "second");
}

/// `environment: {KEY: {secret: NAME}}`: on a system image the variable
/// reaches the supervised command through its 0600 unit env file and never
/// instance config; on an OCI image it is instance config. A new value
/// rolls both.
#[test]
fn stack_env_secret_delivery() {
    if !enabled() {
        return;
    }
    let s = SecretStack::new("env");
    s.deploy(
        &format!(
            "secrets: {{tok: {{environment: ISB_TEST_TOK}}}}\n\
             services:\n\
             \x20 sys:\n    image: {}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s \"$$TOKEN\" > /tmp/t; exec sleep infinity']\n\
             \x20   environment: {{TOKEN: {{secret: tok}}, PLAIN: p}}\n\
             \x20 oci:\n    image: docker:busybox\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s \"$$TOKEN\" > /tmp/t; exec sleep 3600']\n\
             \x20   environment: {{TOKEN: {{secret: tok}}}}\n",
            image()
        ),
        &[("tok", "t0k-value")],
    );
    let org = isb::org::OrgId::default_org();
    // Stored as the stack's own secret.
    let owned = format!("{}_tok", s.name);
    assert_eq!(s.secrets.get(&org, &owned).unwrap().0, b"t0k-value");

    let sys = s.instance("sys");
    wait_file(&sys, "/tmp/t");
    assert_eq!(read(&sys, "/tmp/t"), "t0k-value");
    let mode = sys
        .exec(["stat", "-c", "%a", "/etc/isb/sys.env"])
        .unwrap()
        .stdout_text();
    assert_eq!(mode.trim(), "600");
    let info = sys.info().unwrap();
    assert!(
        !info.config.contains_key("environment.TOKEN"),
        "{:?}",
        info.config
    );
    assert_eq!(
        info.config.get("environment.PLAIN").map(String::as_str),
        Some("p")
    );

    let oci = s.instance("oci");
    assert_eq!(
        oci.info()
            .unwrap()
            .config
            .get("environment.TOKEN")
            .map(String::as_str),
        Some("t0k-value")
    );
    wait_file(&oci, "/tmp/t");
    assert_eq!(read(&oci, "/tmp/t"), "t0k-value");

    // A new value: both services roll to it.
    s.secrets.set(&org, &owned, b"rotated").unwrap();
    assert_eq!(
        s.ctl.secret_changed(&org, &owned),
        std::slice::from_ref(&s.name)
    );
    s.settle();
    let oci2 = s.instance("oci");
    assert_ne!(oci2.name(), oci.name());
    assert_eq!(
        oci2.info()
            .unwrap()
            .config
            .get("environment.TOKEN")
            .map(String::as_str),
        Some("rotated")
    );
    let sys2 = s.instance("sys");
    assert_ne!(sys2.name(), sys.name());
    wait_file(&sys2, "/tmp/t");
    assert_eq!(read(&sys2, "/tmp/t"), "rotated");
}
