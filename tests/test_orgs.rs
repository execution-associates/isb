//! The matching behind the sweep of orphaned test orgs: which org names are
//! a test run's, and when that run is gone. Needs no incusd.

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;

use common::{orphaned_test_org, parse_test_org, start_time_of, stat_start_time};

#[test]
fn parses_only_test_org_names() {
    assert_eq!(parse_test_org("isbt-1234-98765"), Some((1234, Some(98765))));
    assert_eq!(parse_test_org("isbt-1234"), Some((1234, None)));
    for name in [
        "default",
        "ocai",
        "exa",
        "stephan",
        "isbt",
        "isbt-",
        "isbt-x1",
        "isbt-12a",
        "isbt-12-",
        "isbt-12-3-4",
        "isbt-12-abc",
        "isbtest-a123",
        "isbtest-own-123",
        "xisbt-12",
        "isbt-99999999999",
    ] {
        assert_eq!(parse_test_org(name), None, "{name}");
    }
}

#[test]
fn orphaned_when_pid_is_gone_or_reused() {
    let live = |pid: u32| (pid == 10).then_some(500u64);
    assert!(orphaned_test_org("isbt-11-500", live), "pid gone");
    assert!(orphaned_test_org("isbt-10-499", live), "pid reused");
    assert!(!orphaned_test_org("isbt-10-500", live), "still running");
    assert!(orphaned_test_org("isbt-11", live), "old name, pid gone");
    assert!(!orphaned_test_org("isbt-10", live), "old name, pid taken");
    let none = |_: u32| None;
    for name in ["ocai", "default", "isbtest-a1", "isbt12"] {
        assert!(!orphaned_test_org(name, none), "{name}");
    }
}

#[test]
fn reads_start_time_from_stat() {
    let stat = "42 (a (b) c) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 \
                123456 1000 10 18446744073709551615";
    assert_eq!(stat_start_time(stat), Some(123456));
    assert_eq!(stat_start_time("42 (x) S 1"), None);
    let me = std::process::id();
    let start = start_time_of(me).unwrap();
    assert!(!orphaned_test_org(
        &format!("isbt-{me}-{start}"),
        start_time_of
    ));
    assert!(orphaned_test_org(
        &format!("isbt-{me}-{}", start + 1),
        start_time_of
    ));
}
