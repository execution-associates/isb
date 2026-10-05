//! Stack secrets against a real incusd (`ISB_INTEGRATION=1`): delivery as
//! files and variables, and what a new version does per `on_change`.

use std::time::{Duration, Instant};

use isb::{Client, Sandbox};

#[allow(dead_code, reason = "each test binary uses some of the shared helpers")]
mod common;
use common::{enabled, image, test_org_client};

/// A secret store under `state` with a throwaway key.
fn test_secrets(state: &std::path::Path) -> std::sync::Arc<isb::secrets::Secrets> {
    let k = isb::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    std::sync::Arc::new(isb::secrets::Secrets::new(isb::secrets::LocalDriver::new(
        state,
        std::sync::Arc::new(k),
    )))
}

/// Deploys a compose file as a stack on its own controller and store, the
/// way `stack_deploy` does: bind the secrets, then deploy. Removes the stack
/// when dropped.
struct SecretStack {
    ctl: isb::stack::Controller,
    secrets: std::sync::Arc<isb::secrets::Secrets>,
    name: String,
    state: tempfile::TempDir,
}

impl SecretStack {
    fn new(what: &str) -> SecretStack {
        let state = tempfile::tempdir().unwrap();
        let store = isb::stack::Store::open(state.path()).unwrap();
        let secrets = test_secrets(state.path());
        let ctl = isb::stack::Controller::start(
            Client::new(),
            store,
            Duration::from_secs(2),
            secrets.clone(),
        )
        .unwrap();
        SecretStack {
            ctl,
            secrets,
            name: format!("isb-test-{what}{}", std::process::id() % 100000),
            state,
        }
    }

    fn deploy(&self, yaml: &str, given: &[(&str, &str)]) {
        let dir = self.state.path();
        let p = isb::compose::load_docs(
            &[(dir.join("isb.yaml"), yaml.to_string())],
            dir,
            Some(&self.name),
            &|_| None,
        )
        .unwrap();
        let org = common::test_org();
        let given = given
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let secrets =
            isb::stack::secrets::bind(&self.secrets, &org, &self.name, &p.file, &given, false)
                .unwrap();
        let def = isb::stack::StackDef {
            source: None,
            domains: Default::default(),
            name: self.name.clone(),
            org,
            file: p.file,
            base_dir: dir.to_path_buf(),
            secrets,
            force: Default::default(),
            images: Default::default(),
            deployed_at: 0,
            deployed_by: "test".into(),
            previous: None,
        };
        self.ctl.deploy(def).unwrap();
        self.settle();
    }

    fn settle(&self) -> isb::stack::controller::StackStatus {
        let st =
            isb::daemon::wait_settled(&self.ctl, &common::q(&self.name), Duration::from_secs(300))
                .unwrap();
        assert!(st.converged, "{st:?}");
        st
    }

    /// The one instance of a service.
    fn instance(&self, service: &str) -> Sandbox {
        let st = self.ctl.status(&common::q(&self.name)).unwrap();
        let s = st.services.iter().find(|s| s.service == service).unwrap();
        assert_eq!(s.instances.len(), 1, "{s:?}");
        let org_client = test_org_client(&Client::new());
        Sandbox::get(&org_client, &s.instances[0].name).unwrap()
    }
}

impl Drop for SecretStack {
    fn drop(&mut self) {
        let _ = self
            .ctl
            .remove(&common::q(&self.name), true, Duration::from_secs(120));
        self.ctl.shutdown();
    }
}

fn read(sb: &Sandbox, path: &str) -> String {
    let o = sb.exec(["cat", path]).unwrap();
    assert!(o.success(), "cat {path}: {}", o.stderr_text());
    o.stdout_text().trim().to_string()
}

/// Wait until a file in the guest is non-empty.
fn wait_file(sb: &Sandbox, path: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !sb.exec(["test", "-s", path]).is_ok_and(|o| o.success()) {
        assert!(
            Instant::now() < deadline,
            "{}: {path} never written",
            sb.name()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A stack using an `external` secret: the value lands in /run/secrets, and
/// `isb secret set` (the store's set, then the controller told, as the
/// `secret_set` tool does) rolls the service to a new instance holding the
/// new value.
#[test]
fn stack_external_secret_rolls() {
    if !enabled() {
        return;
    }
    let s = SecretStack::new("ext");
    let org = common::test_org();
    let store_name = format!("{}.db", s.name);
    s.secrets
        .create(&org, &store_name, None, b"first", &Default::default())
        .unwrap();
    s.deploy(
        &format!(
            "secrets: {{db: {{external: true, name: {store_name}}}}}\n\
             services:\n  app:\n    image: {}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sleep, infinity]\n    secrets: [db]\n",
            image()
        ),
        &[],
    );
    let before = s.instance("app");
    assert_eq!(read(&before, "/run/secrets/db"), "first");
    // The definition holds the reference, never the value.
    let def = s.ctl.definition(&s.name).unwrap();
    assert_eq!(def.secrets["db"].name, store_name);
    assert_eq!(def.secrets["db"].version, 1);
    assert!(!serde_json::to_string(&def).unwrap().contains("first"));
    let rev = def.revision("app").unwrap();

    s.secrets.set(&org, &store_name, b"second").unwrap();
    assert_eq!(
        isb::stack::secrets::cycled_stacks(&s.ctl.secret_changed(&org, &store_name)),
        std::slice::from_ref(&s.name)
    );
    let st = s.settle();
    assert_ne!(st.services[0].rev, rev);
    let after = s.instance("app");
    assert_ne!(after.name(), before.name(), "a new instance");
    assert_eq!(read(&after, "/run/secrets/db"), "second");
    // An unchanged version rolls nothing.
    assert!(s.ctl.secret_changed(&org, &store_name).is_empty());
    // A reboot restores the file from the guest's own copy.
    after.restart().unwrap();
    wait_file(&after, "/run/secrets/db");
    assert_eq!(read(&after, "/run/secrets/db"), "second");
}

/// `environment: {KEY: {secret: NAME}}`: on a system image the variable
/// reaches the supervised command through its 0600 unit env file and never
/// instance config; on an OCI image it is instance config. A new value
/// rolls both.
#[test]
fn stack_env_secret_delivery() {
    if !enabled() {
        return;
    }
    let s = SecretStack::new("env");
    s.deploy(
        &format!(
            "secrets: {{tok: {{environment: ISB_TEST_TOK}}}}\n\
             services:\n\
             \x20 sys:\n    image: {}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s \"$$TOKEN\" > /tmp/t; exec sleep infinity']\n\
             \x20   environment: {{TOKEN: {{secret: tok}}, PLAIN: p}}\n\
             \x20 oci:\n    image: docker:busybox\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s \"$$TOKEN\" > /tmp/t; exec sleep 3600']\n\
             \x20   environment: {{TOKEN: {{secret: tok}}}}\n",
            image()
        ),
        &[("tok", "t0k-value")],
    );
    let org = common::test_org();
    // Stored as the stack's own secret.
    let owned = format!("{}_tok", s.name);
    assert_eq!(s.secrets.get(&org, &owned).unwrap().0, b"t0k-value");

    let sys = s.instance("sys");
    wait_file(&sys, "/tmp/t");
    assert_eq!(read(&sys, "/tmp/t"), "t0k-value");
    let mode = sys
        .exec(["stat", "-c", "%a", "/etc/isb/sys.env"])
        .unwrap()
        .stdout_text();
    assert_eq!(mode.trim(), "600");
    let info = sys.info().unwrap();
    assert!(
        !info.config.contains_key("environment.TOKEN"),
        "{:?}",
        info.config
    );
    assert_eq!(
        info.config.get("environment.PLAIN").map(String::as_str),
        Some("p")
    );

    let oci = s.instance("oci");
    assert_eq!(
        oci.info()
            .unwrap()
            .config
            .get("environment.TOKEN")
            .map(String::as_str),
        Some("t0k-value")
    );
    wait_file(&oci, "/tmp/t");
    assert_eq!(read(&oci, "/tmp/t"), "t0k-value");

    // A new value: both services roll to it.
    s.secrets.set(&org, &owned, b"rotated").unwrap();
    assert_eq!(
        isb::stack::secrets::cycled_stacks(&s.ctl.secret_changed(&org, &owned)),
        std::slice::from_ref(&s.name)
    );
    s.settle();
    let oci2 = s.instance("oci");
    assert_ne!(oci2.name(), oci.name());
    assert_eq!(
        oci2.info()
            .unwrap()
            .config
            .get("environment.TOKEN")
            .map(String::as_str),
        Some("rotated")
    );
    let sys2 = s.instance("sys");
    assert_ne!(sys2.name(), sys.name());
    wait_file(&sys2, "/tmp/t");
    assert_eq!(read(&sys2, "/tmp/t"), "rotated");
}

/// Wait until `f` holds, for up to two minutes.
fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A stack with three secrets, one per way a new version is taken: `sys` (a
/// system unit) restarts for `f` (its file) and `e` (its variable), `oci`
/// restarts for `e`, and `lazy` takes `n` with `on_change: none`. Returns
/// the stack and the store names of f, e and n.
fn in_place_stack(what: &str) -> (SecretStack, [String; 3]) {
    let s = SecretStack::new(what);
    let org = common::test_org();
    let names = ["f", "e", "n"].map(|k| format!("{}.{k}", s.name));
    for k in &names {
        s.secrets
            .create(&org, k, None, b"one", &Default::default())
            .unwrap();
    }
    let [f, e, n] = &names;
    s.deploy(
        &format!(
            "secrets:\n\
             \x20 f: {{external: true, name: {f}}}\n\
             \x20 e: {{external: true, name: {e}, on_change: restart}}\n\
             \x20 n: {{external: true, name: {n}, on_change: none}}\n\
             services:\n\
             \x20 sys:\n    image: {img}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s-%s \"$$TOKEN\" \"$$(cat /run/secrets/f)\" > /tmp/t; exec sleep infinity']\n\
             \x20   secrets: [{{source: f, on_change: restart}}]\n\
             \x20   environment: {{TOKEN: {{secret: e}}}}\n\
             \x20 lazy:\n    image: {img}\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'cat /run/secrets/n > /tmp/t; exec sleep infinity']\n\
             \x20   secrets: [n]\n\
             \x20 oci:\n    image: docker:busybox\n    labels: {{isb-test: '1'}}\n\
             \x20   command: [sh, -c, 'printf %s \"$$TOKEN\" > /tmp/t; exec sleep 3600']\n\
             \x20   environment: {{TOKEN: {{secret: e}}}}\n",
            img = image()
        ),
        &[],
    );
    for svc in ["sys", "lazy", "oci"] {
        wait_file(&s.instance(svc), "/tmp/t");
    }
    (s, names)
}

/// Every service's revision, in order.
fn revisions(s: &SecretStack) -> Vec<String> {
    let st = s.ctl.status(&common::q(&s.name)).unwrap();
    st.services.iter().map(|x| x.rev.clone()).collect()
}

/// Whether a guest file holds exactly `want`.
fn holds(sb: &Sandbox, path: &str, want: &str) -> bool {
    sb.exec(["cat", path])
        .is_ok_and(|o| o.stdout_text().trim() == want)
}

/// `on_change: restart` keeps each replica's instance and restarts its app
/// with the new value: a system unit for a file and a variable, an OCI
/// instance for a variable.
#[test]
fn stack_secret_on_change_restart() {
    if !enabled() {
        return;
    }
    let (s, [f, e, _]) = in_place_stack("restart");
    let org = common::test_org();
    let (sys, oci) = (s.instance("sys"), s.instance("oci"));
    assert_eq!(read(&sys, "/tmp/t"), "one-one");
    let revs = revisions(&s);

    // f: sys restarts in place with the new file.
    s.secrets.set(&org, &f, b"two").unwrap();
    let c = s.ctl.secret_changed(&org, &f);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(
        (c[0].service.as_str(), c[0].action, c[0].from, c[0].to),
        ("sys", isb::spec::OnChange::Restart, 1, 2)
    );
    wait_until("sys to restart with f=two", || {
        holds(&sys, "/tmp/t", "one-two")
    });

    // e: sys and oci restart in place with the new variable.
    s.secrets.set(&org, &e, b"three").unwrap();
    let mut who: Vec<_> = s
        .ctl
        .secret_changed(&org, &e)
        .iter()
        .map(|c| (c.service.clone(), c.action))
        .collect();
    who.sort();
    let restart = isb::spec::OnChange::Restart;
    assert_eq!(who, [("oci".into(), restart), ("sys".into(), restart)]);
    wait_until("sys to restart with TOKEN=three", || {
        holds(&sys, "/tmp/t", "three-two")
    });
    wait_until("oci to restart with TOKEN=three", || {
        holds(&oci, "/tmp/t", "three")
    });
    let config = oci.info().unwrap().config;
    assert_eq!(
        config.get("environment.TOKEN").map(String::as_str),
        Some("three")
    );
    // Same instances, same revisions: nothing was replaced.
    assert_eq!(s.instance("sys").name(), sys.name());
    assert_eq!(s.instance("oci").name(), oci.name());
    assert_eq!(revisions(&s), revs);
}

/// `on_change: none` updates the file only: the app keeps the old value,
/// and `stack_status` reports the replica stale.
#[test]
fn stack_secret_on_change_none() {
    if !enabled() {
        return;
    }
    let (s, [_, _, n]) = in_place_stack("none");
    let org = common::test_org();
    let lazy = s.instance("lazy");
    let revs = revisions(&s);
    s.secrets.set(&org, &n, b"four").unwrap();
    let c = s.ctl.secret_changed(&org, &n);
    assert_eq!(
        c.iter()
            .map(|c| (c.service.as_str(), c.action))
            .collect::<Vec<_>>(),
        [("lazy", isb::spec::OnChange::None)]
    );
    assert!(isb::stack::secrets::cycled_stacks(&c).is_empty());
    wait_until("lazy to get n=four", || {
        holds(&lazy, "/run/secrets/n", "four")
    });
    assert_eq!(read(&lazy, "/tmp/t"), "one");
    let stale = || {
        let st = s.ctl.status(&common::q(&s.name)).unwrap();
        let l = st.services.iter().find(|x| x.service == "lazy").unwrap();
        l.instances[0].stale_secrets.clone()
    };
    wait_until("lazy to be reported stale", || !stale().is_empty());
    let st = &stale()[0];
    assert_eq!((st.key.as_str(), st.running, st.current), ("n", 1, 2));
    assert_eq!(revisions(&s), revs);
    assert_eq!(s.instance("lazy").name(), lazy.name());
}

/// A secret's `rotate` as a YAML value, `$` escaped from interpolation.
fn rotate_yaml(engine: isb::app::database::Engine, root: bool) -> String {
    serde_json::to_string(&engine.rotate_command(root))
        .unwrap()
        .replace('$', "$$")
}

/// Run `sh -c line` in a guest with `env`, and whether it succeeded.
fn sh_ok(sb: &Sandbox, line: &str, env: &[(&str, &str)]) -> bool {
    let mut o = isb::ExecOptions::default();
    for (k, v) in env {
        o.env.insert(k.to_string(), v.to_string());
    }
    sb.exec_with(["sh", "-c", line], o)
        .is_ok_and(|o| o.success())
}

/// The `rotate` commands database apps get change the password inside a
/// running Postgres, MariaDB (user and root) and Redis before anything is
/// stored, with values that need quoting; a failing one refuses the change.
#[test]
fn stack_secret_rotate_changes_database_passwords() {
    use isb::app::database::Engine;
    if !enabled() {
        return;
    }
    let s = SecretStack::new("rotate");
    let org = common::test_org();
    let n = |k: &str| format!("{}.{k}", s.name);
    for k in ["pg", "my", "myroot", "rd", "bad"] {
        s.secrets
            .create(&org, &n(k), None, b"old-pw", &Default::default())
            .unwrap();
    }
    let sec = |k: &str, rotate: String| {
        format!(
            "  {k}: {{external: true, name: {}, on_change: none, rotate: {rotate}}}\n",
            n(k)
        )
    };
    let yaml = format!(
        "secrets:\n{}{}{}{}{}services:\n\
         \x20 pg:\n    image: docker:postgres:17-alpine\n    labels: {{isb-test: '1'}}\n\
         \x20   environment: {{POSTGRES_USER: app, POSTGRES_DB: app, POSTGRES_PASSWORD: {{secret: pg}}}}\n\
         \x20   healthcheck: {{test: [CMD-SHELL, 'pg_isready -q -h 127.0.0.1 -U app'], interval: 2s, retries: 60, start_period: 120s}}\n\
         \x20 maria:\n    image: docker:mariadb:11\n    labels: {{isb-test: '1'}}\n\
         \x20   environment: {{MARIADB_USER: app, MARIADB_DATABASE: app, MARIADB_PASSWORD: {{secret: my}}, MARIADB_ROOT_PASSWORD: {{secret: myroot}}}}\n\
         \x20   healthcheck: {{test: [CMD-SHELL, 'MYSQL_PWD=\"$$MARIADB_ROOT_PASSWORD\" mariadb-admin ping -h 127.0.0.1 -uroot --silent'], interval: 2s, retries: 60, start_period: 120s}}\n\
         \x20 redis:\n    image: docker:redis:7-alpine\n    labels: {{isb-test: '1'}}\n\
         \x20   command: [sh, -c, 'exec docker-entrypoint.sh redis-server --requirepass \"$$REDIS_PASSWORD\"']\n\
         \x20   environment: {{REDIS_PASSWORD: {{secret: rd}}, REDISCLI_AUTH: {{secret: rd}}}}\n\
         \x20   secrets: [bad]\n",
        sec("pg", rotate_yaml(Engine::Postgres, false)),
        sec("my", rotate_yaml(Engine::Mariadb, false)),
        sec("myroot", rotate_yaml(Engine::Mariadb, true)),
        sec("rd", rotate_yaml(Engine::Redis, false)),
        sec(
            "bad",
            "[sh, -c, 'cat >/dev/null; echo nope >&2; exit 3']".into()
        ),
    );
    s.deploy(&yaml, &[]);
    let (pg, maria, redis) = (s.instance("pg"), s.instance("maria"), s.instance("redis"));
    // Values a careless quote would break.
    let new = r#"n3w'pa"ss\w$x"#;
    let set = |k: &str, v: &str| {
        let applied = s.ctl.rotate_before_set(&org, &n(k), v.as_bytes()).unwrap();
        assert_eq!(applied.len(), 1, "{k}: {applied:?}");
        s.secrets.set(&org, &n(k), v.as_bytes()).unwrap();
        s.ctl.secret_changed(&org, &n(k));
    };
    // Over the bridge address: the image trusts loopback, so only there is
    // the password checked.
    let st = s.ctl.status(&common::q(&s.name)).unwrap();
    let ip = st
        .services
        .iter()
        .find(|x| x.service == "pg")
        .unwrap()
        .instances[0]
        .ip
        .clone()
        .unwrap();
    let psql = format!("psql -h {ip} -U app -d app -tAc 'select 1' | grep -qx 1");
    let psql = psql.as_str();
    assert!(sh_ok(&pg, psql, &[("PGPASSWORD", "old-pw")]));
    set("pg", new);
    assert!(
        sh_ok(&pg, psql, &[("PGPASSWORD", new)]),
        "postgres: new password"
    );
    assert!(
        !sh_ok(&pg, psql, &[("PGPASSWORD", "old-pw")]),
        "postgres: old one gone"
    );

    let my = |user: &str| format!("mariadb -h 127.0.0.1 -u{user} -e 'select 1' >/dev/null");
    set("my", new);
    assert!(
        sh_ok(&maria, &my("app"), &[("MYSQL_PWD", new)]),
        "mariadb user"
    );
    assert!(!sh_ok(&maria, &my("app"), &[("MYSQL_PWD", "old-pw")]));
    // Root's own rotate authenticates with the root password the instance
    // still has.
    set("myroot", "r00t'new");
    assert!(
        sh_ok(&maria, &my("root"), &[("MYSQL_PWD", "r00t'new")]),
        "mariadb root"
    );

    let ping = "redis-cli -h 127.0.0.1 --no-auth-warning ping | grep -q PONG";
    set("rd", new);
    assert!(sh_ok(&redis, ping, &[("REDISCLI_AUTH", new)]), "redis");
    assert!(!sh_ok(&redis, ping, &[("REDISCLI_AUTH", "old-pw")]));

    // A failing rotate refuses the change, naming why.
    let e = s
        .ctl
        .rotate_before_set(&org, &n("bad"), b"x")
        .unwrap_err()
        .to_string();
    assert!(e.contains("exited") && e.contains("nope"), "{e}");
}
