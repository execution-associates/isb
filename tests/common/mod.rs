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

/// The org the stack and app tests deploy into: `isb-test` (incus project
/// `isb-isb-test`), never the default org, which holds a host's own apps.
pub fn test_org() -> isb::org::OrgId {
    isb::org::OrgId::new(TEST_ORG).unwrap()
}

pub const TEST_ORG: &str = "isb-test";

/// The client for [`test_org`]'s incus project. Creates the org with default
/// settings when it is missing and leaves an existing one alone; plain
/// sandboxes stay in incus' `default` project.
pub fn test_org_client(base: &Client) -> Client {
    let org = test_org();
    if isb::org::get(base, &org).is_err() {
        isb::org::ensure(base, &org, &isb::org::OrgOptions::default(), &mut |l| {
            eprintln!("{l}")
        })
        .unwrap();
    }
    isb::org::client(base, &org)
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
