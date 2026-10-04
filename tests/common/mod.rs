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

/// The client for the default org's incus project (`isb-default`), where the
/// default org's stacks and apps live; plain sandboxes stay in incus'
/// `default` project. Creates the org the way `isb serve` does when it is
/// missing, and leaves an existing one alone.
pub fn default_org_client(base: &Client) -> Client {
    isb::org::ensure_default(base, &mut |l| eprintln!("{l}")).unwrap();
    isb::org::client(base, &isb::org::OrgId::default_org())
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
