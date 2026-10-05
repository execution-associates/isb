//! Helpers shared by the integration test files.

use isb::{Client, Sandbox};

/// Whether to run against incusd: only with `ISB_INTEGRATION=1`.
pub fn enabled() -> bool {
    if std::env::var("ISB_INTEGRATION").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set ISB_INTEGRATION=1 to run against incusd");
    false
}

/// The image the tests create instances from.
pub fn image() -> String {
    std::env::var("ISB_TEST_IMAGE").unwrap_or_else(|_| "dev-base".into())
}

/// The org the stack and app tests deploy into: `isbt-<pid>-<start>`, one
/// per test process (`<start>` is the process's start time, so a reused pid
/// never names a live run's org), never the default org, which holds a
/// host's own apps. Made on first use and deleted, with everything left in
/// it, when the process exits or is stopped by SIGINT, SIGTERM or SIGHUP.
/// A run killed outright (SIGKILL, a crash) leaves its org behind, so first
/// use also sweeps the orgs of test processes that are gone
/// ([`sweep_test_orgs`]).
pub fn test_org() -> isb::org::OrgId {
    static ORG: std::sync::OnceLock<isb::org::OrgId> = std::sync::OnceLock::new();
    ORG.get_or_init(|| {
        let pid = std::process::id();
        let start = start_time_of(pid).expect("this process's start time from /proc");
        let org = isb::org::OrgId::new(format!("isbt-{pid}-{start}")).unwrap();
        // SAFETY: registers a plain function with libc, run once at exit.
        unsafe { libc::atexit(remove_test_org) };
        on_stop_signal(remove_test_org);
        // Held until the org is whole, so a signal meanwhile waits for it
        // rather than leaving a half-made org behind.
        let mut created = CREATED.lock().unwrap_or_else(|e| e.into_inner());
        let base = Client::new();
        sweep_test_orgs(&base);
        isb::org::ensure(&base, &org, &isb::org::OrgOptions::default(), &mut |l| {
            eprintln!("{l}")
        })
        .unwrap();
        *created = Some(org.clone());
        org
    })
    .clone()
}

/// The org [`test_org`] made, until it is removed.
static CREATED: std::sync::Mutex<Option<isb::org::OrgId>> = std::sync::Mutex::new(None);

/// Remove [`test_org`]'s org, once, whichever of exit or a signal comes
/// first. The lock is held throughout: a run whose org goes on a signal
/// fails its tests and exits, and that exit must wait for the removal
/// rather than end the process halfway through it.
extern "C" fn remove_test_org() {
    let mut created = CREATED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(org) = created.take() {
        if let Err(e) = isb::org::remove(&Client::new(), &org, true, &mut |_| {}) {
            eprintln!("cleanup: org {org}: {e}");
        }
    }
}

/// Run `cleanup` when a stop signal (SIGINT, SIGTERM, SIGHUP) arrives, then
/// die of that signal as if it had not been caught. The handler only writes
/// the signal number to a pipe (removing an org talks to incusd, which no
/// signal handler may do); a thread reads it, puts the default actions back
/// (so a second Ctrl-C kills at once, leaving the org to the next sweep),
/// cleans up and re-raises.
fn on_stop_signal(cleanup: extern "C" fn()) {
    const SIGNALS: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];
    static PIPE_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    extern "C" fn handler(sig: i32) {
        let b = sig as u8;
        let fd = PIPE_W.load(std::sync::atomic::Ordering::Relaxed);
        // SAFETY: write(2) is async-signal-safe; one byte to a pipe we own.
        unsafe { libc::write(fd, (&raw const b).cast(), 1) };
    }
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 fills the two-element array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!("cleanup: no pipe for signals; a stopped run leaves its org to the next sweep");
        return;
    }
    PIPE_W.store(fds[1], std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        // SAFETY: the read end is ours alone from here on.
        let mut r = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut b = [0u8; 1];
        if r.read_exact(&mut b).is_err() {
            return;
        }
        let sig = i32::from(b[0]);
        for s in SIGNALS {
            // SAFETY: restores the default action.
            unsafe { libc::signal(s, libc::SIG_DFL) };
        }
        eprintln!("cleanup: signal {sig}: removing the test org");
        cleanup();
        // SAFETY: the default action is back, so this ends the process.
        unsafe { libc::raise(sig) };
    });
    for s in SIGNALS {
        // SAFETY: the handler only calls write(2).
        unsafe { libc::signal(s, handler as extern "C" fn(i32) as libc::sighandler_t) };
    }
}

/// Remove the orgs of test runs that are gone: every [`orphaned_test_org`]
/// among the host's orgs, by name, each logged to stderr, and then the
/// pieces of one killed while it was being made (its names directory,
/// bridge and ACL, which come before its project). Nothing that does not
/// match the test pattern is ever touched. Errors are logged, not fatal:
/// another process may be sweeping the same org.
pub fn sweep_test_orgs(base: &Client) {
    let orgs = match isb::org::names(base) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("sweep: listing orgs: {e}");
            return;
        }
    };
    for org in &orgs {
        if !orphaned_test_org(org.as_str(), start_time_of) {
            continue;
        }
        eprintln!("sweep: removing org {org}, left by a test run that is gone");
        if let Err(e) = isb::org::remove(base, org, true, &mut |_| {}) {
            eprintln!("sweep: org {org}: {e}");
        }
    }
    let host = base.clone().project("default");
    let half_made = |name: &str| {
        orphaned_test_org(name, start_time_of) && !orgs.iter().any(|o| o.as_str() == name)
    };
    let delete = |path: String, what: String| {
        eprintln!("sweep: deleting {what}, left by a test run that is gone");
        let deadline = host.get_timeouts().other;
        if let Err(e) = host.mutate("DELETE", &path, None, &what, deadline) {
            eprintln!("sweep: {what}: {e}");
        }
    };
    let list = |path: &str| match host.get(path) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => {
            eprintln!("sweep: listing {path}: {e}");
            Vec::new()
        }
    };
    // A bridge's name is a hash; its DNS domain names the org.
    for n in list("/1.0/networks?recursion=1") {
        let (Some(name), Some(domain)) = (n["name"].as_str(), n["config"]["dns.domain"].as_str())
        else {
            continue;
        };
        if domain.strip_suffix(".isb").is_some_and(half_made) {
            delete(
                format!("/1.0/networks/{name}"),
                format!("network {name} ({domain})"),
            );
        }
    }
    for a in list("/1.0/network-acls") {
        let Some(acl) = a.as_str().and_then(|p| p.rsplit('/').next()) else {
            continue;
        };
        if acl.strip_prefix("isb-").is_some_and(half_made) {
            delete(format!("/1.0/network-acls/{acl}"), format!("ACL {acl}"));
        }
    }
    let dirs = std::fs::read_dir(isb::discovery::root())
        .into_iter()
        .flatten();
    for name in dirs
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        if let Ok(org) = isb::org::OrgId::new(name.as_str()) {
            if half_made(&name) {
                eprintln!(
                    "sweep: deleting the names directory of {org}, left by a test run that is gone"
                );
                isb::discovery::remove_org(&org);
            }
        }
    }
}

/// A test org's pid and, when the name has one, its process's start time:
/// `isbt-<pid>-<start>`, or a bare `isbt-<pid>` with no start time.
/// Anything else is not a test org.
pub fn parse_test_org(name: &str) -> Option<(u32, Option<u64>)> {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    let rest = name.strip_prefix("isbt-")?;
    let (pid, start) = match rest.split_once('-') {
        Some((p, s)) => (p, Some(s)),
        None => (rest, None),
    };
    if !digits(pid) || !start.is_none_or(digits) {
        return None;
    }
    let start = match start {
        Some(s) => Some(s.parse().ok()?),
        None => None,
    };
    Some((pid.parse().ok()?, start))
}

/// Whether `name` is a test org whose process is gone: `start_of(pid)` (the
/// start time of a live process, `None` when there is none) says nothing
/// runs as that pid, or something started at another time does (the pid
/// was reused). An old-style name without a start time is orphaned only
/// when its pid is free, since a reused pid cannot be told from its run.
pub fn orphaned_test_org(name: &str, start_of: impl Fn(u32) -> Option<u64>) -> bool {
    let Some((pid, start)) = parse_test_org(name) else {
        return false;
    };
    match (start, start_of(pid)) {
        (_, None) => true,
        (Some(s), Some(live)) => s != live,
        (None, Some(_)) => false,
    }
}

/// The start time of a live process (field 22 of `/proc/<pid>/stat`, in
/// clock ticks since boot), or `None` when there is no such process.
pub fn start_time_of(pid: u32) -> Option<u64> {
    stat_start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Field 22, `starttime`, of a `/proc/<pid>/stat` line. The command name
/// (field 2) is in parentheses and may hold spaces or parentheses itself,
/// so fields are counted from the last `)`.
pub fn stat_start_time(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

/// A test stack's controller key: `isb-test/NAME`.
pub fn q(stack: &str) -> String {
    isb::stack::qualified(&test_org(), stack)
}

/// The client for [`test_org`]'s incus project; plain sandboxes stay in
/// incus' `default` project.
pub fn test_org_client(base: &Client) -> Client {
    isb::org::client(base, &test_org())
}

/// A VM's host bind mount at `/mnt/share` is translated by virtiofsd: guest root
/// (the mapped id, since the spec has no `user:`) writes as whoever runs the
/// test, any other guest id is refused, and root can neither chown to a third
/// id nor make a device node. A setuid bit may be set, but only on a file the
/// invoker owns.
pub fn vm_share_is_translated(sb: &Sandbox, share: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let me = std::fs::metadata(share).unwrap();
    let out = sb
        .exec([
            "sh",
            "-c",
            "echo back > /mnt/share/from-vm; \
             cp /bin/true /mnt/share/suid && chmod 4755 /mnt/share/suid; \
             chown 1234:1234 /mnt/share/suid 2>/dev/null && echo CHOWN-OK; \
             mknod /mnt/share/dev c 1 3 2>/dev/null && echo MKNOD-OK; \
             setpriv --reuid=1234 --regid=1234 --clear-groups \
               touch /mnt/share/other 2>/dev/null && echo OTHER-OK; true",
        ])
        .unwrap();
    let o = out.stdout_text();
    assert!(
        !o.contains("-OK"),
        "guest escalated through the share: {o} {}",
        out.stderr_text()
    );
    assert_eq!(
        std::fs::read_to_string(share.join("from-vm")).unwrap(),
        "back\n"
    );
    for f in ["from-vm", "suid"] {
        let m = std::fs::metadata(share.join(f)).unwrap();
        assert_eq!((m.uid(), m.gid()), (me.uid(), me.gid()), "{f}");
    }
    assert!(!share.join("dev").exists());
    assert!(!share.join("other").exists());
}

/// A stack as the tests deploy it: `file` with no secrets, by `test`.
pub fn test_def(
    name: &str,
    org: isb::org::OrgId,
    file: isb::spec::ComposeFile,
    base_dir: &std::path::Path,
) -> isb::stack::StackDef {
    isb::stack::StackDef {
        source: None,
        domains: Default::default(),
        name: name.to_string(),
        org,
        file,
        base_dir: base_dir.to_path_buf(),
        secrets: Default::default(),
        force: Default::default(),
        images: Default::default(),
        deployed_at: 0,
        deployed_by: "test".into(),
        previous: None,
    }
}
