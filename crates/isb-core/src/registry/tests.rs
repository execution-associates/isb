//! Tests of the registry: references, retention, deployed images and gc.

use super::*;

fn d(n: u8) -> String {
    oci::digest_of(&[n])
}

#[test]
fn refs_parse_and_never_name_another_org() {
    let r = ImageRef::parse("web:v1").unwrap();
    assert_eq!((r.app.as_str(), r.tag.as_deref()), ("web", Some("v1")));
    let r = ImageRef::parse("web").unwrap();
    assert_eq!(r.tag_or_latest(), "latest");
    let dg = d(1);
    let r = ImageRef::parse(&format!("web:v2@{dg}")).unwrap();
    assert_eq!(r.digest.as_deref(), Some(dg.as_str()));
    assert_eq!(r.render(), format!("web:v2@{dg}"));
    let org = OrgId::new("acme").unwrap();
    assert_eq!(r.pull_alias(&org), format!("acme/web@{dg}"));
    assert_eq!(
        ImageRef::parse("web:v1").unwrap().pull_alias(&org),
        "acme/web:v1"
    );
    assert_eq!(
        ImageRef::parse("web:v1").unwrap().pinned(&dg).render(),
        format!("web:v1@{dg}")
    );
    for bad in [
        "other/web:v1",
        "../web",
        "Web",
        "web:bad tag",
        "web@sha256:abc",
        "",
        "web:",
        ":v1",
        "web:-x",
    ] {
        assert!(ImageRef::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn retention_keeps_newest_and_deployed() {
    let t = |tag: &str, n: u8, at: u64| TagInfo {
        tag: tag.into(),
        digest: d(n),
        pushed_at: at,
    };
    let tags = vec![
        t("v1", 1, 100),
        t("v2", 2, 200),
        t("v3", 3, 300),
        t("v4", 4, 400),
        // Same image as v4 under another, older tag: v4 keeps it.
        t("old", 4, 50),
        t("unknown", 5, 0),
    ];
    let none = BTreeSet::new();
    let del = select_deletions(&tags, 2, &none);
    assert_eq!(del, vec![d(2), d(1), d(5)]);
    // A deployed digest survives however old.
    let prot: BTreeSet<String> = [d(1)].into();
    assert_eq!(select_deletions(&tags, 2, &prot), vec![d(2), d(5)]);
    assert!(select_deletions(&tags, 10, &none).is_empty());
    assert_eq!(select_deletions(&tags, 0, &none).len(), 5);
    // Retention's own tags are not among the newest, and go once what
    // they kept is no longer deployed.
    let mut with_keep = tags.clone();
    with_keep.push(t(&format!("{KEEP_TAG}000000000001"), 1, 999));
    assert_eq!(select_deletions(&with_keep, 2, &prot), vec![d(2), d(5)]);
    assert_eq!(
        select_deletions(&with_keep, 2, &none),
        vec![d(2), d(1), d(5)]
    );
    // Preview tags are never among the newest: kept while deployed,
    // deleted once not.
    let mut with_pr = tags.clone();
    with_pr.push(t("pr-7-abc", 8, 9999));
    with_pr.push(t("pr-7-def", 9, 9998));
    let prot9: BTreeSet<String> = [d(9)].into();
    assert_eq!(
        select_deletions(&with_pr, 2, &prot9),
        vec![d(2), d(1), d(5), d(8)]
    );
    assert!(is_preview_tag("pr-12-0123abc"));
    assert_eq!(preview_tag_number("pr-12-0123abc"), Some(12));
    for t in ["pr-x-abc", "pr-12", "pr--a", "v1", "pr-12-"] {
        assert!(!is_preview_tag(t), "{t}");
    }
}

#[test]
fn deployed_images_are_protected() {
    let dg = d(7);
    let file: crate::spec::ComposeFile = serde_yaml_ng::from_str(&format!(
        "services:\n  web: {{image: 'registry:web:v2'}}\n  api: {{image: 'registry:api@{dg}'}}\n  db: {{image: 'docker:postgres'}}\n"
    ))
    .unwrap();
    let mut def = crate::stack::StackDef {
        source: None,
        domains: Default::default(),
        name: "s".into(),
        org: OrgId::new("acme").unwrap(),
        file: file.clone(),
        base_dir: "/".into(),
        secrets: Default::default(),
        force: Default::default(),
        images: [("web".to_string(), d(2))].into(),
        deployed_at: 0,
        deployed_by: String::new(),
        previous: None,
    };
    let mut prev = def.clone();
    prev.images = [("web".to_string(), d(1))].into();
    def.previous = Some(Box::new(prev));
    let p = protected_by(&[Arc::new(def)]);
    assert!(p.contains(&format!("acme/web@{}", d(2))));
    assert!(
        p.contains(&format!("acme/web@{}", d(1))),
        "the previous deployment"
    );
    assert!(p.contains(&format!("acme/api@{dg}")));
    assert_eq!(p.len(), 3);
}

#[test]
fn gc_against_a_fake_registry() {
    let (base, st) = oci::tests::fake();
    let remote = oci::Remote::new(&base, None, Duration::from_secs(10)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry {
        base: Client::with_socket("/nonexistent"),
        info: Info {
            addr: base.trim_start_matches("http://").into(),
            ca_pem: String::new(),
        },
        remote,
        dir: Some(dir.path().join("registry")),
        lock: Mutex::new(()),
    };
    let org = OrgId::new("acme").unwrap();
    let mut digests = Vec::new();
    for i in 0..3u8 {
        let (t, _) = oci::tests::image_tar(&[i; 10]);
        let p = dir.path().join(format!("{i}.tar"));
        std::fs::write(&p, t).unwrap();
        digests.push(
            reg.push(&org, "web", &format!("v{i}"), &p, &mut |_| {})
                .unwrap(),
        );
        // Distinct push times, newest last.
        let mut idx = reg.load_index();
        idx.repos
            .get_mut("acme/web")
            .unwrap()
            .get_mut(&format!("v{i}"))
            .unwrap()
            .1 = 1000 + i as u64;
        reg.save_index(&idx).unwrap();
    }
    let ls = reg.list(Some(&org)).unwrap();
    assert_eq!(
        ls[0]
            .tags
            .iter()
            .map(|t| t.tag.as_str())
            .collect::<Vec<_>>(),
        ["v2", "v1", "v0"]
    );
    assert!(
        reg.list(Some(&OrgId::new("other").unwrap()))
            .unwrap()
            .is_empty()
    );
    let r = reg
        .resolve(&org, &ImageRef::parse("web:v1").unwrap())
        .unwrap();
    assert_eq!(r, digests[1]);
    assert!(
        reg.resolve(
            &OrgId::new("other").unwrap(),
            &ImageRef::parse("web:v1").unwrap()
        )
        .is_err()
    );
    // Dry: keep 1, and v0 is deployed.
    let prot: BTreeSet<String> = [format!("acme/web@{}", digests[0])].into();
    let rep = reg.gc(1, &prot, true, &mut |_| {}).unwrap();
    assert_eq!(rep.deleted.len(), 1);
    assert!(rep.deleted[0].contains(&digests[1]) && rep.deleted[0].contains("(v1)"));
    assert!(
        st.lock()
            .unwrap()
            .log
            .iter()
            .all(|l| !l.starts_with("DELETE"))
    );
}
