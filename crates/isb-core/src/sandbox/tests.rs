use super::*;

#[test]
fn default_route_parsing() {
    let v4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\neth0\t00000000\t0100B40A\t0003\t0\t0\t0\t00000000\t0\t0\t0\neth0\t0000B40A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
    assert!(has_default_route(v4, ""));
    let no = "Iface\tDestination\tGateway \tFlags\neth0\t0000B40A\t00000000\t0001\n";
    assert!(!has_default_route(no, ""));
    let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
    assert!(has_default_route(no, v6));
    let v6_lo_only = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
    assert!(!has_default_route(no, v6_lo_only));
}

#[test]
fn label_filters() {
    let mut i =
        SandboxInfo::from_api(&json!({"name": "a", "config": {"user.k": "v", "user.p": "/x"}}));
    assert!(i.matches(&[LabelFilter::parse("k")]));
    assert!(i.matches(&[LabelFilter::parse("k=v")]));
    assert!(!i.matches(&[LabelFilter::parse("k=w")]));
    assert!(!i.matches(&[LabelFilter::parse("missing")]));
    assert!(i.matches(&[LabelFilter::parse("k=v"), LabelFilter::parse("p")]));
    i.labels.clear();
    assert!(i.matches(&[]));
}
