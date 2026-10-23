//! isbx against a real incusd (and the host's `isb serve`, for egress).
//!
//! Gated like the other integration tests: only with `ISB_INTEGRATION=1`.
//! isbx names its own sandboxes (`x-run-<hash>`, `x-<tool>-<n>`), so these
//! tests make them from a temporary project and a per-process tool name,
//! and delete exactly those afterwards, pass or fail. isbx's config and
//! shims go to temporary directories, never the user's.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::enabled;
use isb::sandbox::LabelFilter;
use isb::{Client, Sandbox};

fn isb_bin() -> PathBuf {
    std::env::var_os("ISB_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_isb")))
}

/// A project directory, `isbx` on a PATH of its own, and isbx's config and
/// shims under the same temporary directory. Its sandboxes go when dropped.
struct Rig {
    dir: tempfile::TempDir,
    label: String,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(isb_bin(), bin.join("isbx")).unwrap();
        std::os::unix::fs::symlink(isb_bin(), bin.join("isb")).unwrap();
        let root = root.canonicalize().unwrap();
        Rig {
            label: format!("isbx.root={}", root.display()),
            dir,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("proj").canonicalize().unwrap()
    }

    fn cmd(&self, prog: &str, cwd: &Path) -> Command {
        let mut c = Command::new(self.dir.path().join("bin").join(prog));
        let path = format!(
            "{}:{}:{}",
            self.dir.path().join("data/isb/x/bin").display(),
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        c.current_dir(cwd)
            .env("PATH", path)
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .stdin(Stdio::null());
        c
    }

    fn isbx(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd("isbx", cwd).arg("-q").args(args).output().unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let c = Client::new();
        for i in Sandbox::list_with(&c, &[LabelFilter::parse(&self.label)]).unwrap_or_default() {
            if let Err(e) = Sandbox::remove(&c, &i.name, true) {
                eprintln!("cleanup: {}: {e}", i.name);
            }
        }
    }
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_one_off_command_feels_like_the_host_and_reaches_nothing_of_it() {
    if !enabled() {
        return;
    }
    let r = Rig::new();
    let root = r.root();
    let sub = root.join("sub");

    // The first command of a new sandbox resolves names: no DNS race.
    let o = r.isbx(&sub, &["getent", "hosts", "example.com"]);
    assert!(o.status.success(), "{o:?}");

    // Same path inside, the caller's directory, the command's exit code.
    let o = r.isbx(&sub, &["pwd"]);
    assert_eq!(text(&o).trim(), sub.display().to_string());
    assert_eq!(r.isbx(&sub, &["sh", "-c", "exit 7"]).status.code(), Some(7));

    // stdin is forwarded; stdout and stderr stay apart without a terminal.
    let mut child = r
        .cmd("isbx", &sub)
        .args(["-q", "sh", "-c", "tr a-z A-Z; echo err >&2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    let o = child.wait_with_output().unwrap();
    assert_eq!(out, "HELLO");
    assert!(String::from_utf8_lossy(&o.stderr).contains("err"));

    // Files written inside are the caller's on the host.
    assert!(
        r.isbx(&sub, &["sh", "-c", "echo hi > out.txt"])
            .status
            .success()
    );
    let meta = std::fs::metadata(sub.join("out.txt")).unwrap();
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions.
    assert_eq!(meta.uid(), unsafe { libc::getuid() });

    // .git is read-only: no hook can be planted.
    let o = r.isbx(&root, &["sh", "-c", "echo x > .git/hooks/pre-commit"]);
    assert!(!o.status.success());
    assert!(!root.join(".git/hooks/pre-commit").exists());

    // Only display variables pass, and -e KEY names another.
    let mut c = r.cmd("isbx", &sub);
    c.args(["-q", "sh", "-c", "echo \"[$ISBX_T_SECRET][$HOME]\""])
        .env("ISBX_T_SECRET", "s3cret");
    assert_eq!(text(&c.output().unwrap()).trim(), "[][/home/dev]");
    let mut c = r.cmd("isbx", &sub);
    c.args([
        "-q",
        "-e",
        "ISBX_T_SECRET",
        "sh",
        "-c",
        "echo \"[$ISBX_T_SECRET]\"",
    ])
    .env("ISBX_T_SECRET", "s3cret");
    assert_eq!(text(&c.output().unwrap()).trim(), "[s3cret]");

    // The host's home is never a root.
    let home = std::env::var("HOME").unwrap();
    let o = r.isbx(&sub, &["--root", &home, "true"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("refusing to mount"));

    // All of it ran in one kept sandbox.
    let c = Client::new();
    let mine = Sandbox::list_with(&c, &[LabelFilter::parse(&r.label)]).unwrap();
    assert_eq!(mine.len(), 1, "{mine:?}");
}

#[test]
fn a_tool_installs_runs_from_its_shim_updates_and_goes() {
    if !enabled() {
        return;
    }
    let r = Rig::new();
    let root = r.root();
    let name = format!("t{}", std::process::id());
    let hello = format!(
        "sh:printf '#!/bin/sh\\necho hello \"$*\"\\n' | sudo tee /usr/local/bin/{name} >/dev/null && sudo chmod +x /usr/local/bin/{name}"
    );
    let label = format!("isbx.tool={name}");
    struct Gone(String);
    impl Drop for Gone {
        fn drop(&mut self) {
            let c = Client::new();
            for i in Sandbox::list_with(&c, &[LabelFilter::parse(&self.0)]).unwrap_or_default() {
                let _ = Sandbox::remove(&c, &i.name, true);
            }
        }
    }
    let _gone = Gone(label.clone());

    let o = r
        .cmd("isb", &root)
        .args([
            "x", "install", &name, "--mount", "none", "--from", &hello, "-q",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let shim = r.dir.path().join("data/isb/x/bin").join(&name);
    assert!(
        std::fs::read_to_string(&shim)
            .unwrap()
            .contains("# isbx shim")
    );

    // The shim is a plain host command: it finds `isb` on PATH.
    let o = Command::new(&shim)
        .args(["a", "b"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                r.dir.path().join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("XDG_CONFIG_HOME", r.dir.path().join("config"))
        .env("XDG_DATA_HOME", r.dir.path().join("data"))
        .current_dir(&root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(text(&o).trim(), "hello a b", "{o:?}");

    // An update builds generation 2 and drops generation 1.
    let o = r
        .cmd("isb", &root)
        .args(["x", "update", &name, "-q"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    let c = Client::new();
    let gens: Vec<String> = Sandbox::list_with(&c, &[LabelFilter::parse(&label)])
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert_eq!(gens, [format!("x-{name}-2")]);

    // A failed update leaves the working generation in use.
    let def = r.dir.path().join(format!("config/isb/x/tools/{name}.yaml"));
    let t = std::fs::read_to_string(&def).unwrap();
    std::fs::write(&def, t.replace(&hello, "sh:exit 3")).unwrap();
    let o = r
        .cmd("isb", &root)
        .args(["x", "update", &name, "-q"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(
        std::fs::read_to_string(&def)
            .unwrap()
            .contains(&format!("x-{name}-2"))
    );
    assert!(Sandbox::get(&c, &format!("x-{name}-2")).is_ok());

    let o = r
        .cmd("isb", &root)
        .args(["x", "rm", &name, "-q"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{o:?}");
    assert!(!shim.exists() && !def.exists());
    assert!(
        Sandbox::list_with(&c, &[LabelFilter::parse(&label)])
            .unwrap()
            .is_empty()
    );
}
