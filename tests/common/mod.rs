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

/// The org the stack and app tests deploy into: `isbt-<pid>`, one per test
/// process, never the default org, which holds a host's own apps. Made on
/// first use and deleted, with everything left in it, when the process
/// exits, so no test org outlives its run.
pub fn test_org() -> isb::org::OrgId {
    static ORG: std::sync::OnceLock<isb::org::OrgId> = std::sync::OnceLock::new();
    ORG.get_or_init(|| {
        let org = isb::org::OrgId::new(format!("isbt-{}", std::process::id())).unwrap();
        let base = Client::new();
        isb::org::ensure(&base, &org, &isb::org::OrgOptions::default(), &mut |l| {
            eprintln!("{l}")
        })
        .unwrap();
        extern "C" fn remove_test_org() {
            if let Some(org) = ORG.get() {
                if let Err(e) = isb::org::remove(&Client::new(), org, true, &mut |_| {}) {
                    eprintln!("cleanup: org {org}: {e}");
                }
            }
        }
        unsafe extern "C" {
            fn atexit(f: extern "C" fn()) -> i32;
        }
        // SAFETY: registers a plain function with libc, run once at exit.
        unsafe { atexit(remove_test_org) };
        org
    })
    .clone()
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
