use super::*;
use crate::client::Client;
use crate::stack::Controller;
use crate::template::catalog::Catalogs;

fn templates(dir: &Path) -> Templates {
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
        dir,
        Arc::new(k),
    )));
    let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
    let store = crate::stack::Store::open(dir).unwrap();
    let ctl = Controller::start(
        client.clone(),
        store,
        Duration::from_secs(60),
        secrets.clone(),
    )
    .unwrap();
    let apps = Apps::new(dir, client, ctl, secrets.clone())
        .with_digest(Arc::new(|_: &str| None))
        .with_timeout(Duration::from_secs(5));
    Templates::new(dir, apps, secrets, Some("203.0.113.7".parse().unwrap())).with(
        Catalogs::with_fetch(dir, Arc::new(|_: &str| Err(Error::invalid("offline")))),
        Arc::new(|_: &str| Ok(None)),
    )
}

fn deploy_args(v: Value) -> DeployArgs {
    serde_json::from_value(v).unwrap()
}

#[test]
fn deploys_and_removes_an_instance() {
    let dir = tempfile::tempdir().unwrap();
    let t = templates(dir.path());
    let org = OrgId::new("acme").unwrap();
    let local = Caller::Local { uid: None };
    // A dry run changes nothing and shows no secret value.
    let dry = t
        .deploy(
            &org,
            deploy_args(json!({"template": "umami", "project": "web", "dry_run": true})),
            &local,
        )
        .unwrap();
    assert_eq!(dry["plan"]["order"], json!(["umami-db", "umami"]));
    assert!(t.apps.project_get(&org, "web").is_err());
    assert!(t.secrets.list(&org).unwrap().is_empty());
    // The real thing: project made, secrets and apps created, record kept.
    let r = t
        .deploy(
            &org,
            deploy_args(json!({"template": "builtin/umami", "project": "web", "wait": true, "timeout": "20s"})),
            &local,
        )
        .unwrap();
    assert_eq!(r["instance"]["apps"], json!(["umami-db", "umami"]));
    assert!(t.apps.get(&org, "umami").is_ok());
    let names: Vec<String> = t
        .secrets
        .list(&org)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    for n in [
        "tpl.umami.db_password",
        "tpl.umami.app_secret",
        "tpl.umami.umami.env.DATABASE_URL",
    ] {
        assert!(names.iter().any(|x| x == n), "{n} in {names:?}");
    }
    let meta = t.secrets.inspect(&org, "tpl.umami.db_password").unwrap();
    assert_eq!(meta.labels["isb.template.instance"], "umami");
    // No incusd here: the first deploy fails and the rest are not tried.
    assert_eq!(r["deployments"].as_array().unwrap().len(), 1, "{r}");
    // The app record holds a reference, never the value.
    let (pw, _) = t.secrets.get(&org, "tpl.umami.db_password").unwrap();
    let app = std::fs::read_to_string(dir.path().join("orgs/acme/apps/umami-db/app.json")).unwrap();
    assert!(!app.contains(std::str::from_utf8(&pw).unwrap()));
    assert!(app.contains("tpl.umami.db_password"));
    // A second one under the same name is refused.
    let again = t
        .deploy(
            &org,
            deploy_args(json!({"template": "umami", "project": "web", "dry_run": true})),
            &local,
        )
        .unwrap();
    assert!(again["error"].as_str().unwrap().contains("already exist"));
    assert!(
        t.deploy(
            &org,
            deploy_args(json!({"template": "umami", "project": "web"})),
            &local
        )
        .is_err()
    );
    assert_eq!(t.instances(&org).unwrap().len(), 1);
    // Removing takes the apps, the secrets and the record.
    let gone = t.remove(&org, "umami").unwrap();
    assert_eq!(gone["apps"], json!(["umami", "umami-db"]));
    assert!(t.apps.get(&org, "umami").is_err());
    assert!(
        t.secrets
            .list(&org)
            .unwrap()
            .iter()
            .all(|s| !s.name.starts_with("tpl."))
    );
    assert!(t.instances(&org).unwrap().is_empty());
    assert!(t.remove(&org, "umami").is_err());
}

#[test]
fn a_refused_template_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let cat = dir.path().join("dok/blueprints/bad");
    std::fs::create_dir_all(&cat).unwrap();
    std::fs::write(
        cat.join("docker-compose.yml"),
        "services:\n  bad:\n    image: x\n    privileged: true\n",
    )
    .unwrap();
    let t = templates(dir.path());
    t.catalogs
        .add(CatalogConfig {
            name: "dok".into(),
            format: Format::Dokploy,
            location: dir.path().join("dok").display().to_string(),
        })
        .unwrap();
    let d = t.describe("dok/bad").unwrap();
    assert_eq!(d["compatibility"]["status"], "refused");
    let e = t
        .deploy(
            &OrgId::new("acme").unwrap(),
            deploy_args(json!({"template": "dok/bad", "project": "p"})),
            &Caller::Local { uid: None },
        )
        .unwrap_err();
    assert!(e.to_string().contains("privileged"), "{e}");
}
