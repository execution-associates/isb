//! The `isb exec` CLI against a real incusd: exit codes, stdin (forwarded
//! from a pipe or file by default, dropped by `-n`, never holding up exit
//! when it stays open), SIGINT, and a terminal.
//!
//! Gated by `ISB_INTEGRATION=1`, as tests/integration.rs: the one instance it
//! makes is named `isb-test-*` and removed afterwards, pass or fail.
//! `ISB_TEST_IMAGE` picks the image, `ISB_BIN` the `isb` binary.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use isb::spec::{IdmapMode, IdmapSpec};
use isb::{Client, Sandbox, SandboxSpec};

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;
use common::{enabled, image};

/// Deletes the test's sandbox when dropped.
struct Cleanup {
    client: Client,
    name: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        match Sandbox::remove(&self.client, &self.name, true) {
            Ok(()) => {}
            Err(e) if e.is_not_found() => {}
            Err(e) => eprintln!("cleanup: {}: {e}", self.name),
        }
    }
}

/// A fresh sandbox (removed when the guard drops) and the `isb` binary.
fn sandbox(what: &str) -> (Cleanup, String, PathBuf) {
    let client = Client::new();
    let name = format!("isb-test-{}-{what}", std::process::id());
    let cleanup = Cleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let spec = SandboxSpec::new(&name, image())
        .cpus(2)
        .memory("1GiB")
        .idmap(IdmapSpec::Mode(IdmapMode::Auto))
        .label("isb-test", "1");
    Sandbox::create(&client, &spec).expect("create");
    let bin = std::env::var_os("ISB_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_isb")));
    (cleanup, name, bin)
}

/// Exit codes, SIGINT, and a terminal.
#[test]
fn cli_exec() {
    if !enabled() {
        return;
    }
    let (_cleanup, name, bin) = sandbox("cli");
    let st = Command::new(&bin)
        .args(["exec", &name, "--", "sh", "-c", "exit 3"])
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(3));

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

/// Stdin: a pipe or file is forwarded by default (and with `-i`), `-n` drops
/// it, and a pipe that never closes does not hold up a command that never
/// reads it.
#[test]
fn cli_exec_stdin() {
    if !enabled() {
        return;
    }
    let (_cleanup, name, bin) = sandbox("cli-stdin");
    // A stdin pipe that never reaches EOF (the parent keeps it open and never
    // writes) must not hang a command that does not read it: with no flags
    // (piped stdin is forwarded by default) and with -T.
    for flags in [&[][..], &["-T"][..]] {
        let t = Instant::now();
        let mut child = Command::new(&bin)
            .arg("exec")
            .args(flags)
            .args([&name, "--", "echo", "fine"])
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
        assert_eq!((st.code(), out.as_str()), (Some(0), "fine\n"), "{flags:?}");
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "{flags:?} took {:?}",
            t.elapsed()
        );
    }

    // Piped stdin reaches the command with no flag, and with -i (a no-op
    // for a pipe); EOF closes it.
    for flags in [&[][..], &["-i"][..]] {
        let mut child = Command::new(&bin)
            .arg("exec")
            .args(flags)
            .args([&name, "--", "tr", "a-z", "A-Z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"hello").unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{flags:?}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "HELLO", "{flags:?}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), "", "{flags:?}");
    }
    // A file on stdin: `isb exec NAME -- bash -s < script.sh`.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script.sh");
    std::fs::write(&script, "echo from-script\nexit 5\n").unwrap();
    let o = Command::new(&bin)
        .args(["exec", &name, "--", "sh", "-s"])
        .stdin(std::fs::File::open(&script).unwrap())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(5));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "from-script\n");
    // -n drops it: the command sees EOF, and isb says nothing.
    let mut child = Command::new(&bin)
        .args(["exec", "-n", &name, "--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}
