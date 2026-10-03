use super::*;

fn meta(id: &str) -> Meta {
    Meta {
        id: id.into(),
        name: id.into(),
        ..Default::default()
    }
}

fn tr(compose: &str, toml: &str) -> (Option<Template>, Report) {
    translate(&meta("t"), compose, toml)
}

#[test]
fn helpers_parse() {
    let vars: BTreeMap<String, String> = [
        ("sec".to_string(), String::new()),
        ("pay".into(), String::new()),
    ]
    .into_iter()
    .collect();
    let p = |s: &str| parse_helper(s, &vars);
    assert_eq!(p("domain"), Some(Helper::Domain));
    assert_eq!(p("password"), Some(Helper::Password(16)));
    assert_eq!(p("password:32"), Some(Helper::Password(32)));
    assert_eq!(p("base64:64"), Some(Helper::Base64(64)));
    assert_eq!(p("base64"), Some(Helper::Base64(32)));
    assert_eq!(p("hash:12"), Some(Helper::Hash(12)));
    assert_eq!(p("uuid"), Some(Helper::Uuid));
    assert_eq!(p("randomPort"), Some(Helper::RandomPort));
    assert_eq!(p("jwt:32"), Some(Helper::JwtHex(32)));
    assert_eq!(
        p("jwt:sec:pay"),
        Some(Helper::Jwt {
            secret: Some("sec".into()),
            payload: Some("pay".into())
        })
    );
    assert_eq!(
        p("timestamps:2030-01-01T00:00:00Z"),
        Some(Helper::Timestamp {
            ms: false,
            at: Some("2030-01-01T00:00:00Z".into())
        })
    );
    assert_eq!(p("jwt:nope"), None, "an unknown secret variable");
    assert_eq!(p("HOME"), None);
    // References, helpers and literal text; unknown ${...} stays literal.
    assert_eq!(
        dparse("https://${sec}/${password:8}${X}", &vars),
        vec![
            DPart::Lit("https://".into()),
            DPart::Ref("sec".into()),
            DPart::Lit("/".into()),
            DPart::Helper(Helper::Password(8)),
            DPart::Lit("${X}".into()),
        ]
    );
}

#[test]
fn compose_interpolation() {
    let env: BTreeMap<String, String> = [
        ("A".to_string(), "${a}".to_string()),
        ("E".to_string(), String::new()),
    ]
    .into_iter()
    .collect();
    let mut unset = BTreeSet::new();
    let mut i = |s: &str| interpolate(s, &env, &mut unset).unwrap();
    assert_eq!(i("x ${A} $A $$B $"), "x ${a} ${a} $$B $$");
    assert_eq!(i("${E:-d}|${E-d}|${N:-d${A}}"), "d||d${a}");
    assert_eq!(i("${A:+yes}${N:+no}"), "yes");
    assert_eq!(i("${N}"), "");
    assert!(unset.contains("N"));
}

#[test]
fn image_refs() {
    assert_eq!(image_ref("postgres:16"), "docker:postgres:16");
    assert_eq!(
        image_ref("louislam/uptime-kuma:1"),
        "docker:louislam/uptime-kuma:1"
    );
    assert_eq!(
        image_ref("docker.io/library/redis:7"),
        "docker:library/redis:7"
    );
    assert_eq!(image_ref("ghcr.io/a/b:v1"), "ghcr:a/b:v1");
    assert_eq!(image_ref("quay.io/a/b"), "quay:a/b");
    assert_eq!(
        image_ref("docker.n8n.io/n8nio/n8n"),
        "oci:docker.n8n.io/n8nio/n8n"
    );
    assert_eq!(image_ref("localhost:5000/x"), "oci:localhost:5000/x");
}

const PLAUSIBLE_LIKE: &str = r#"
services:
  plausible_db:
    image: postgres:16-alpine
    restart: always
    volumes: [db-data:/var/lib/postgresql/data]
    environment:
      - POSTGRES_PASSWORD=${POSTGRES_PASSWORD}
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
  events:
    image: clickhouse/clickhouse-server:24.12-alpine
    restart: always
    volumes:
      - event-data:/var/lib/clickhouse
      - ../files/clickhouse/logging.xml:/etc/clickhouse-server/config.d/logging.xml:ro
    ulimits:
      nofile: {soft: 262144, hard: 262144}
  plausible:
    image: ghcr.io/plausible/community-edition:v3.0.1
    restart: always
    command: sh -c "sleep 10 && /entrypoint.sh run"
    depends_on: [plausible_db, events]
    volumes: [plausible-data:/var/lib/plausible]
    environment:
      - DATABASE_URL=postgres://postgres:${POSTGRES_PASSWORD}@plausible_db:5432/plausible_db
      - CLICKHOUSE_DATABASE_URL=http://events:8123/plausible_events_db
    env_file: [.env]
    expose: [8000]
volumes:
  db-data: {driver: local}
  event-data: {}
  plausible-data: {}
"#;

const PLAUSIBLE_TOML: &str = r#"
[variables]
main_domain = "${domain}"
secret_base = "${base64:64}"
pg_pass = "${password:32}"

[config]
[[config.domains]]
serviceName = "plausible"
port = 8_000
host = "${main_domain}"

[config.env]
BASE_URL = "http://${main_domain}"
SECRET_KEY_BASE = "${secret_base}"
POSTGRES_PASSWORD = "${pg_pass}"
INLINE = "${password:12}"

[[config.mounts]]
filePath = "/clickhouse/logging.xml"
content = """
<clickhouse><logger><level>warning</level></logger></clickhouse>
"""
"#;

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn translates_a_multi_service_template() {
    let (t, r) = tr(PLAUSIBLE_LIKE, PLAUSIBLE_TOML);
    assert_eq!(r.status, Status::Notes, "{r:?}");
    let t = t.expect("translated");
    let keys: Vec<&str> = t.apps.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(keys, ["plausible-db", "events", "plausible"]);
    assert_eq!(t.main.as_deref(), Some("plausible"));
    let var = |n: &str| t.variables.iter().find(|v| v.name == n).unwrap().clone();
    assert_eq!(var("main_domain").kind, VarKind::Domain);
    assert_eq!(var("secret_base").kind, VarKind::Base64);
    assert_eq!(var("secret_base").bytes, Some(64));
    assert_eq!(var("pg_pass").length, Some(32));
    // An inline helper in config.env became its own variable.
    assert_eq!(var("env_inline").kind, VarKind::Password);
    let app = |k: &str| t.apps.iter().find(|a| a.name == k).unwrap().clone();
    let p = app("plausible");
    assert_eq!(p.image, "ghcr:plausible/community-edition:v3.0.1");
    assert_eq!(p.depends_on, ["plausible-db", "events"]);
    // .env entries via env_file, interpolation into environment, and the
    // other services' names rewritten.
    assert_eq!(p.env["BASE_URL"], "http://${main_domain}");
    assert_eq!(
        p.env["DATABASE_URL"],
        "postgres://postgres:${pg_pass}@${host:plausible-db}:5432/plausible_db"
    );
    assert_eq!(
        p.env["CLICKHOUSE_DATABASE_URL"],
        "http://${host:events}:8123/plausible_events_db"
    );
    // docker's command after the image's entrypoint.
    assert_eq!(
        p.args,
        Some(json!(["sh", "-c", "sleep 10 && /entrypoint.sh run"]))
    );
    assert!(p.command.is_none());
    assert_eq!(p.domains[0]["host"], json!("${main_domain}"));
    assert_eq!(p.domains[0]["port"], json!(8000));
    assert_eq!(p.port, Some(8000));
    let e = app("events");
    assert_eq!(e.files.len(), 1);
    assert_eq!(
        e.files[0].path,
        "/etc/clickhouse-server/config.d/logging.xml"
    );
    assert!(e.files[0].content.contains("<level>warning</level>"));
    assert_eq!(e.volumes, ["event-data:/var/lib/clickhouse"]);
    assert!(
        r.notes.iter().any(|n| n.contains("ulimits")),
        "{:?}",
        r.notes
    );
    assert!(
        r.notes.iter().any(|n| n.contains("rewritten")),
        "{:?}",
        r.notes
    );
    // And it plans: the whole chain works.
    let ep = |_: &str| -> crate::error::Result<Option<Vec<String>>> {
        Ok(Some(vec!["/entrypoint.sh".into()]))
    };
    let pl = super::super::plan(
        &t,
        &super::super::Params {
            org: crate::org::OrgId::new("acme").unwrap(),
            project: "web".into(),
            environment: "production".into(),
            instance: "plausible".into(),
            values: Default::default(),
        },
        &super::super::Context {
            public_ip: Some("203.0.113.9".parse().unwrap()),
            entrypoint: &ep,
            free_port: &|| Some(20001),
        },
    )
    .unwrap();
    let names: Vec<&str> = pl.apps.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["plausible-db", "plausible-events", "plausible"]);
    assert_eq!(pl.order[2], "plausible");
    let pa = serde_json::to_value(&pl.apps[2]).unwrap();
    assert_eq!(
        pa["command"],
        json!([
            "/entrypoint.sh",
            "sh",
            "-c",
            "sleep 10 && /entrypoint.sh run"
        ])
    );
    assert!(
        pa["env"]
            .as_str()
            .unwrap()
            .contains("DATABASE_URL=${{secret.tpl.plausible.plausible.env.DATABASE_URL}}"),
        "{}",
        pa["env"]
    );
}

#[test]
fn refuses_what_weakens_isolation() {
    let cases = [
        ("privileged: true", "privileged"),
        ("cap_add: [NET_ADMIN]", "capabilities"),
        ("devices: [/dev/net/tun]", "devices"),
        ("network_mode: host", "network_mode host"),
        ("pid: host", "process namespace"),
        (
            "volumes: [/var/run/docker.sock:/var/run/docker.sock]",
            "docker socket",
        ),
        ("volumes: [/etc:/host-etc:ro]", "host path /etc"),
        ("build: .", "built from source"),
        (
            "extra_hosts: [host.docker.internal:host-gateway]",
            "extra_hosts",
        ),
        ("ports: [\"51820:51820/udp\"]", "udp"),
        ("user: www-data", "numeric"),
        ("restart: \"no\"", "one-shot"),
        ("sysctls: {net.ipv4.ip_forward: 1}", "sysctls"),
    ];
    for (line, why) in cases {
        let compose =
            format!("services:\n  app:\n    image: x/y:1\n    restart: always\n    {line}\n");
        let compose = compose.replace("    restart: always\n    restart:", "    restart:");
        let (t, r) = tr(&compose, "");
        assert!(t.is_none(), "{line}");
        assert_eq!(r.status, Status::Refused, "{line}");
        assert!(
            r.refusals.iter().any(|x| x.contains(why)),
            "{line}: {:?}",
            r.refusals
        );
    }
    // A volume shared by two services.
    let (t, r) = tr(
        "services:\n  a:\n    image: x\n    restart: always\n    volumes: [d:/d]\n  b:\n    image: y\n    restart: always\n    volumes: [d:/e]\nvolumes:\n  d: {}\n",
        "",
    );
    assert!(t.is_none());
    assert!(r.refusals[0].contains("shared by a, b"), "{:?}", r.refusals);
    // Harmless spellings are not refused.
    let (t, r) = tr(
        "services:\n  a:\n    image: x\n    restart: always\n    privileged: false\n    cap_add: []\n",
        "",
    );
    assert!(t.is_some(), "{r:?}");
}

#[test]
fn traefik_labels_become_domains() {
    let compose = r#"
services:
  web:
    image: nginx
    restart: always
    labels:
      - traefik.enable=true
      - traefik.http.routers.web.rule=Host(`${DOMAIN}`) && PathPrefix(`/app`)
      - traefik.http.routers.web.entrypoints=websecure
      - traefik.http.routers.web.middlewares=strip@docker
      - traefik.http.middlewares.strip.stripprefix.prefixes=/app
      - traefik.http.services.web.loadbalancer.server.port=8080
      - com.example.other=1
"#;
    let toml =
        "[variables]\nmain_domain = \"${domain}\"\n[config.env]\nDOMAIN = \"${main_domain}\"\n";
    let (t, r) = tr(compose, toml);
    let t = t.unwrap_or_else(|| panic!("{r:?}"));
    let d = &t.apps[0].domains[0];
    assert_eq!(d["host"], json!("${main_domain}"));
    assert_eq!(d["port"], json!(8080));
    assert_eq!(d["path"], json!("/app"));
    assert_eq!(d["strip_prefix"], json!(true));
    assert!(d.get("https").is_none());
    assert!(r.notes.iter().any(|n| n.contains("labels are not applied")));
    // Plain HTTP entrypoint, and a middleware isb has no equivalent for.
    let plain = compose
        .replace("websecure", "web")
        .replace("strip@docker", "")
        .replace(" && PathPrefix(`/app`)", "");
    let (t, _) = tr(&plain, toml);
    assert_eq!(t.unwrap().apps[0].domains[0]["https"], json!(false));
    let auth = compose.replace("stripprefix.prefixes=/app", "basicauth.users=a:b");
    let (t, r) = tr(&auth, toml);
    assert!(t.is_none());
    assert!(
        r.refusals.iter().any(|x| x.contains("middleware strip")),
        "{r:?}"
    );
    let regex = compose.replace(
        "Host(`${DOMAIN}`) && PathPrefix(`/app`)",
        "HostRegexp(`.+`)",
    );
    let (t, _) = tr(&regex, toml);
    assert!(t.is_none());
}

#[test]
fn host_rewriting_is_conservative() {
    let svc = vec![
        ("db".to_string(), "db".to_string()),
        ("redis".into(), "cache".into()),
    ];
    let rw = |s: &str, whole: bool| rewrite_hosts(s, &svc, whole).0;
    assert_eq!(
        rw("postgres://u:p@db:5432/x", false),
        "postgres://u:p@${host:db}:5432/x"
    );
    assert_eq!(rw("redis://redis", false), "redis://${host:cache}");
    assert_eq!(rw("db:5432", false), "${host:db}:5432");
    assert_eq!(rw("db", true), "${host:db}");
    // Not a host position: left alone.
    assert_eq!(rw("db", false), "db");
    assert_eq!(
        rw("mydb:5432 db_name db.example.com", false),
        "mydb:5432 db_name db.example.com"
    );
    assert_eq!(rw("${x}@db:1", false), "${x}@${host:db}:1");
    assert!(hostish_key("DB_HOST") && hostish_key("REDIS_URL") && !hostish_key("POSTGRES_USER"));
}

#[test]
fn volumes_ports_and_commands() {
    let compose = r#"
services:
  app:
    image: x
    restart: unless-stopped
    entrypoint: ["/bin/run"]
    command: --flag ${PORT}
    user: "1000:1000"
    working_dir: /srv
    volumes:
      - ./data:/data
      - /cache
      - type: volume
        source: named
        target: /named
        read_only: true
      - type: tmpfs
        target: /tmp/x
    ports:
      - 3000
      - "8080:80"
    deploy:
      replicas: 2
      resources: {limits: {cpus: "0.5", memory: 512M}}
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:${PORT}"]
      interval: 10s
      retries: 3
volumes:
  named: {}
"#;
    let (t, r) = tr(compose, "[config.env]\nPORT = \"9000\"\n");
    let t = t.unwrap_or_else(|| panic!("{r:?}"));
    let a = &t.apps[0];
    assert_eq!(a.command, Some(json!(["/bin/run", "--flag", "9000"])));
    assert_eq!(a.user.as_deref(), Some("1000:1000"));
    assert_eq!(a.working_dir.as_deref(), Some("/srv"));
    assert_eq!(
        a.volumes,
        ["data:/data", "anon-1:/cache", "named:/named:ro"]
    );
    assert_eq!(a.ports, ["127.0.0.1:8080:80"]);
    assert_eq!(a.replicas, Some(2));
    let res = a.resources.as_ref().unwrap();
    assert_eq!(res.cpus.as_deref(), Some("1"));
    assert_eq!(res.memory.as_deref(), Some("512m"));
    assert_eq!(
        a.healthcheck.as_ref().unwrap()["test"],
        json!(["CMD", "curl", "-f", "http://localhost:9000"])
    );
    for n in [
        "bind ./data",
        "anonymous volume",
        "tmpfs",
        "without a host port",
        "127.0.0.1 only",
        "rounded up",
    ] {
        assert!(r.notes.iter().any(|x| x.contains(n)), "{n}: {:?}", r.notes);
    }
}

#[test]
fn a_clean_template_has_no_notes() {
    let (t, r) = tr(
        "services:\n  uptime-kuma:\n    image: louislam/uptime-kuma:1\n    restart: always\n    volumes:\n      - uptime-kuma-data:/app/data\n    expose: [3001]\nvolumes:\n  uptime-kuma-data: {}\n",
        "[variables]\nmain_domain = \"${domain}\"\n[config]\nenv = {}\nmounts = []\n[[config.domains]]\nserviceName = \"uptime-kuma\"\nport = 3_001\nhost = \"${main_domain}\"\n",
    );
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let t = t.unwrap();
    assert_eq!(t.apps[0].volumes, ["uptime-kuma-data:/app/data"]);
    assert_eq!(t.variables.len(), 1);
}

/// The compatibility report over a checkout of Dokploy/templates:
/// `ISB_DOKPLOY_DIR=.../blueprints cargo test --lib dokploy_catalog -- --ignored --nocapture`.
#[test]
#[ignore]
fn dokploy_catalog() {
    let Ok(dir) = std::env::var("ISB_DOKPLOY_DIR") else {
        eprintln!("ISB_DOKPLOY_DIR is not set");
        return;
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut notes: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    let mut ids: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    ids.sort_by_key(|e| e.file_name());
    for e in ids {
        let id = e.file_name().to_string_lossy().to_string();
        let Ok(compose) = std::fs::read_to_string(e.path().join("docker-compose.yml")) else {
            continue;
        };
        let toml = std::fs::read_to_string(e.path().join("template.toml")).unwrap_or_default();
        let (_, r) = translate(&meta(&id), &compose, &toml);
        let s = match r.status {
            Status::Clean => "clean",
            Status::Notes => "notes",
            Status::Refused => "refused",
        };
        *counts.entry(s).or_default() += 1;
        // Group reasons by their kind: the text after "<service>: ".
        let kind = |x: &str| {
            let k = x.split_once(": ").map(|(_, b)| b).unwrap_or(x);
            let k = k.split(" (").next().unwrap_or(k);
            k.split_whitespace().take(5).collect::<Vec<_>>().join(" ")
        };
        let mut seen = BTreeSet::new();
        for x in &r.refusals {
            if seen.insert(kind(x)) {
                *reasons.entry(kind(x)).or_default() += 1;
            }
        }
        let mut seen = BTreeSet::new();
        for x in &r.notes {
            if seen.insert(kind(x)) {
                *notes.entry(kind(x)).or_default() += 1;
            }
        }
        lines.push(format!("{id}\t{s}\t{}", r.refusals.join(" | ")));
    }
    if let Ok(out) = std::env::var("ISB_DOKPLOY_REPORT") {
        std::fs::write(out, lines.join("\n")).unwrap();
    }
    println!("COUNTS {counts:?}");
    let mut r: Vec<_> = reasons.into_iter().collect();
    r.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("TOP REFUSALS (templates)");
    for (k, n) in r.iter().take(25) {
        println!("{n:5}  {k}");
    }
    let mut n: Vec<_> = notes.into_iter().collect();
    n.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("TOP NOTES (templates)");
    for (k, c) in n.iter().take(25) {
        println!("{c:5}  {k}");
    }
}
