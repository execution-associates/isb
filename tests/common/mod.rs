//! Helpers shared by the integration test files.

use isb::Client;

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
