use super::tests::*;
use super::*;
use crate::template::VarKind;
use crate::template::shared::Status;

// --- variables --------------------------------------------------------------

#[test]
fn defaults_requirements_and_plain_references() {
    let (t, r) = ok(r#"
services:
  app:
    image: app:${TAG:-1.2}
    restart: always
    environment:
      - A=${A_VAR:-fallback}
      - B=$B_VAR
      - C=${C_VAR}
      - D=${D_VAR:?}
      - E=${E_VAR:?must be set}
      - F=${F_VAR-dash}
      - G=${G_VAR:-pre-${A_VAR}-post}
      - H=a$$b
      - API_TOKEN=${API_TOKEN:-changeme}
      - EMPTY
"#);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let env = &t.apps[0].env;
    assert_eq!(t.apps[0].image, "docker:app:${tag}");
    assert_eq!(var(&t, "tag").default.as_deref(), Some("1.2"));
    assert_eq!(var(&t, "a_var").default.as_deref(), Some("fallback"));
    assert_eq!(env["A"], "${a_var}");
    // A variable with no default is optional and empty.
    assert_eq!(var(&t, "b_var").default.as_deref(), Some(""));
    assert_eq!(var(&t, "c_var").default.as_deref(), Some(""));
    assert_eq!(var(&t, "b_var").required, None);
    // `:?` is required, and has no default.
    for n in ["d_var", "e_var"] {
        assert_eq!(var(&t, n).required, Some(true), "{n}");
        assert_eq!(var(&t, n).default, None, "{n}");
    }
    assert_eq!(var(&t, "f_var").default.as_deref(), Some("dash"));
    assert_eq!(
        var(&t, "g_var").default.as_deref(),
        Some("pre-${a_var}-post")
    );
    assert_eq!(env["H"], "a$$b", "$$ is a literal $ in both");
    // A name that says "token", with a default, is a secret.
    assert_eq!(var(&t, "api_token").secret, Some(true));
    assert_eq!(var(&t, "a_var").secret, None);
    // A bare name is that variable.
    assert_eq!(env["EMPTY"], "${empty}");
    assert_eq!(var(&t, "empty").default.as_deref(), Some(""));
    assert!(plan_it(&t).is_ok());
}

#[test]
fn the_first_default_wins_and_says_so() {
    let (t, r) = ok(r#"
services:
  a:
    image: a:1
    restart: always
    environment:
      - X=${V:-one}
  b:
    image: b:1
    restart: always
    environment:
      - X=${V:-two}
      - Y=${V:-one}
"#);
    assert_eq!(var(&t, "v").default.as_deref(), Some("one"));
    assert_eq!(r.status, Status::Notes);
    assert!(r.notes[0].contains("different defaults"), "{r:?}");
    // A default after a bare use is still the default.
    let (t, _) = ok(&svc("    environment:\n      - X=$V\n      - Y=${V:-d}\n"));
    assert_eq!(var(&t, "v").default.as_deref(), Some("d"));
}

#[test]
fn a_required_variable_with_a_default_somewhere_is_optional() {
    let (t, _) = ok(&svc(
        "    environment:\n      - X=${V:?}\n      - Y=${V:-d}\n",
    ));
    assert_eq!(var(&t, "v").required, None);
    assert_eq!(var(&t, "v").default.as_deref(), Some("d"));
}

#[test]
fn conditional_interpolation_is_refused() {
    let r = refusals(&svc("    environment:\n      - X=${V:+yes}\n"));
    assert!(r[0].contains("conditional interpolation"), "{r:?}");
    let r = refusals(&svc("    environment:\n      - X=${1x}\n"));
    assert!(r[0].contains("is not a variable"), "{r:?}");
}

#[test]
fn variable_names_are_native_and_unique() {
    let (t, _) = ok(&svc(
        "    environment:\n      - A=$FOO_BAR\n      - B=$foo_bar\n      - C=${_APP_X}\n",
    ));
    assert!(t.var("foo_bar").is_some() && t.var("foo_bar_2").is_some());
    assert!(t.var("v__app_x").is_some(), "{:?}", t.variables);
    assert!(t.validate().is_ok());
}

#[test]
fn a_dotted_environment_name_is_refused() {
    let r = refusals(&svc(
        "    environment:\n      - discovery.type=single-node\n",
    ));
    assert!(
        r[0].contains("environment variable \"discovery.type\""),
        "{r:?}"
    );
}

// --- files ------------------------------------------------------------------

#[test]
fn inline_content_becomes_a_file() {
    let (t, r) = ok(r##"
services:
  web:
    image: web:1
    restart: always
    environment:
      - SECRET=$SERVICE_PASSWORD_WEB
    volumes:
      - type: bind
        source: ./nginx.conf
        target: /etc/nginx/nginx.conf
        content: |
          server { listen 80; server_name $host; location / { proxy_set_header Host $http_host; } }
          secret ${SERVICE_PASSWORD_WEB} and $SERVICE_USER_WEB, plain $$HOME and ${NOT_KNOWN}
      - type: bind
        source: ./run.sh
        target: /run.sh
        content: "#!/bin/sh\necho hi\n"
      - type: bind
        source: ./empty
        target: /data/empty
        isDirectory: false
        content: ""
"##);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let f = &t.apps[0].files;
    assert_eq!(f.len(), 3);
    assert_eq!(f[0].path, "/etc/nginx/nginx.conf");
    let c = &f[0].content;
    // Nginx variables and shell escapes stay as written.
    assert!(c.contains("server_name $$host;"), "{c}");
    assert!(c.contains("Host $$http_host;"), "{c}");
    assert!(c.contains("plain $$$$HOME"), "{c}");
    assert!(c.contains("$${NOT_KNOWN}"), "{c}");
    // Magic names are replaced.
    assert!(c.contains("secret ${password_web} and ${user_web},"), "{c}");
    assert_eq!(f[0].mode, None);
    // A script is executable.
    assert_eq!(f[1].mode.as_deref(), Some("0555"));
    assert_eq!(f[2].content, "");
    assert!(t.apps[0].volumes.is_empty(), "a file is not a volume");
    assert!(plan_it(&t).is_ok());
}

#[test]
fn content_expands_variables_the_compose_file_uses() {
    let (t, _) = ok(r#"
services:
  web:
    image: web:1
    restart: always
    environment:
      - PORT=${WEB_PORT:-8080}
    volumes:
      - type: bind
        source: ./c
        target: /c.conf
        content: "listen ${WEB_PORT}; other ${WEB_PORT:-9}"
"#);
    assert_eq!(
        t.apps[0].files[0].content,
        "listen ${web_port}; other ${web_port}"
    );
}

#[test]
fn a_file_target_must_be_absolute() {
    let r = refusals(&svc(
        "    volumes:\n      - {type: bind, source: ./x, target: rel/x, content: hi}\n",
    ));
    assert!(r[0].contains("not an absolute path"), "{r:?}");
}

// --- volumes ----------------------------------------------------------------

#[test]
fn volumes_become_named_volumes() {
    let (t, r) = ok(r#"
services:
  db:
    image: db:1
    restart: always
    volumes:
      - db-data:/var/lib/db
      - db-conf:/etc/db:ro
      - /var/lib/anon
      - type: volume
        source: dirs
        target: /dirs
        is_directory: true
      - ./local:/local
volumes:
  db-data:
  db-conf:
  dirs:
"#);
    assert_eq!(
        t.apps[0].volumes,
        [
            "db-data:/var/lib/db",
            "db-conf:/etc/db:ro",
            "anon-1:/var/lib/anon",
            "dirs:/dirs",
            "local:/local",
        ]
    );
    assert_eq!(r.status, Status::Notes);
    assert!(
        r.notes.iter().any(|n| n.contains("anonymous volume")),
        "{r:?}"
    );
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("bind ./local becomes the named volume local")),
        "{r:?}"
    );
}

#[test]
fn what_would_weaken_isolation_is_refused() {
    let one = |extra: &str| refusals(&svc(extra)).join(" | ");
    assert!(
        one("    volumes: ['/var/run/docker.sock:/var/run/docker.sock']\n")
            .contains("runtime socket")
    );
    assert!(
        one("    volumes: ['/run/user/1000/podman/podman.sock:/p.sock']\n")
            .contains("runtime socket")
    );
    assert!(one("    volumes: ['/srv/data:/data']\n").contains("host path /srv/data"));
    assert!(one("    volumes: ['~/books:/books']\n").contains("host path"));
    assert!(one("    volumes: ['${DATA:-/data}:/data']\n").contains("host path"));
    assert!(one("    privileged: true\n").contains("privileged"));
    assert!(one("    cap_add: [NET_ADMIN]\n").contains("added capabilities"));
    assert!(one("    network_mode: host\n").contains("network_mode host"));
    assert!(one("    network_mode: 'service:vpn'\n").contains("network_mode"));
    assert!(one("    pid: host\n").contains("process namespace"));
    assert!(one("    devices: ['/dev/dri:/dev/dri']\n").contains("host devices"));
    assert!(one("    sysctls: {net.ipv4.ip_forward: 1}\n").contains("sysctls"));
    assert!(one("    volumes_from: [b]\n").contains("volumes_from"));
    assert!(one("    ports: ['53:53/udp']\n").contains("udp"));
    assert!(one("    user: postgres\n").contains("must be numeric"));
    let r = refusals("services:\n  a:\n    build: .\n").join(" | ");
    assert!(
        r.contains("no image") || r.contains("built from source"),
        "{r}"
    );
    let r = refusals("services:\n  a:\n    image: a:1\n    restart: 'no'\n").join(" | ");
    assert!(r.contains("one-shot"), "{r}");
}

#[test]
fn harmless_values_of_refused_keys_pass() {
    let (_, r) = ok(&svc(
        "    privileged: false\n    cap_add: []\n    network_mode: bridge\n",
    ));
    assert_eq!(r.status, Status::Clean, "{r:?}");
}

#[test]
fn a_volume_shared_by_two_services_is_refused() {
    let r = refusals(
        r#"
services:
  web:
    image: a:1
    restart: always
    volumes: [uploads:/u]
  worker:
    image: a:1
    restart: always
    volumes: [uploads:/u]
"#,
    );
    assert!(
        r[0].contains("volume uploads is shared by web, worker"),
        "{r:?}"
    );
}

#[test]
fn a_one_shot_dependency_is_refused() {
    let r = refusals(
        r#"
services:
  app:
    image: a:1
    restart: always
    depends_on:
      migrate:
        condition: service_completed_successfully
  migrate:
    image: a:1
    restart: always
"#,
    );
    assert!(r[0].contains("waits for migrate to complete"), "{r:?}");
}

#[test]
fn compose_that_does_not_parse_is_refused() {
    assert!(refusals("services: [")[0].contains("does not parse"));
    assert!(refusals("version: '3'\n")[0].contains("no services"));
    assert!(refusals("- a\n- b\n")[0].contains("not a mapping"));
}

// --- what is carried over -----------------------------------------------------

#[test]
fn health_checks_dependencies_and_commands_are_carried() {
    let (t, r) = ok(SEARXNG);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let web = &t.apps[0];
    assert_eq!(web.depends_on, ["redis"]);
    let h = web.healthcheck.as_ref().unwrap();
    assert_eq!(
        h["test"],
        serde_json::json!([
            "CMD",
            "wget",
            "-q",
            "--spider",
            "http://127.0.0.1:8080/healthz"
        ])
    );
    assert_eq!(h["interval"], "5s");
    assert_eq!(h["retries"], 3);
    let (t, _) = ok(&svc(
        "    entrypoint: [\"/bin/sh\", \"-c\"]\n    command: \"echo $$HOME\"\n    user: \"1000:1000\"\n    working_dir: /srv\n    exclude_from_hc: true\n",
    ));
    let a = &t.apps[0];
    assert_eq!(
        a.command,
        Some(serde_json::json!(["/bin/sh", "-c", "echo", "$$HOME"]))
    );
    assert_eq!(a.user.as_deref(), Some("1000:1000"));
    assert_eq!(a.working_dir.as_deref(), Some("/srv"));
}

#[test]
fn other_services_in_host_positions_are_rewritten() {
    let (t, _) = ok(UMAMI);
    assert_eq!(
        t.apps[0].env["DATABASE_URL"],
        "postgres://${user_postgres}:${password_postgres}@${host:postgresql}:5432/${postgres_db}"
    );
    // Not in a host position: left alone.
    let (t, _) = ok(
        "services:\n  db:\n    image: db:1\n    restart: always\n  app:\n    image: a:1\n    restart: always\n    environment:\n      - NAME=db\n      - TEXT=the db is fine\n      - DB_HOST=db\n",
    );
    let env = &t.apps[1].env;
    assert_eq!(env["NAME"], "db");
    assert_eq!(env["TEXT"], "the db is fine");
    assert_eq!(env["DB_HOST"], "${host:db}");
}

#[test]
fn notes_say_what_changes() {
    let (t, r) = ok(r#"
services:
  a:
    image: a:1
    ports: ['8000:80']
    platform: linux/amd64
    container_name: aaa
    ulimits: {nofile: 1}
    labels: {x: y}
    env_file: .env
    environment:
      - TZ=UTC
"#);
    assert_eq!(r.status, Status::Notes);
    let all = r.notes.join("\n");
    for w in [
        "127.0.0.1 only",
        "platform is ignored",
        "container_name",
        "ulimits",
        "labels",
        "env_file",
    ] {
        assert!(all.contains(w), "{w}: {all}");
    }
    assert_eq!(t.apps[0].ports, ["127.0.0.1:8000:80"]);
    // No restart policy is Coolify's default, so it is not a note.
    assert!(!all.contains("restart"), "{all}");
    // A published port can come from a variable.
    let (t, _) = ok(&svc("    ports: ['${PORT:-8000}:80']\n"));
    assert_eq!(t.apps[0].ports, ["127.0.0.1:${port}:80"]);
    assert!(plan_it(&t).is_ok());
}

// --- real templates -----------------------------------------------------------

#[test]
fn umami_translates_cleanly() {
    let m = meta("umami", UMAMI, Some("https://example.com/coolify"));
    let (t, r) = tr_meta(&m, UMAMI);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    assert_eq!(t.id, "umami");
    assert_eq!(t.name, "Umami");
    assert_eq!(
        t.logo.as_deref(),
        Some("https://example.com/coolify/public/svgs/umami.svg")
    );
    assert_eq!(t.links["docs"], "https://umami.is");
    assert_eq!(t.main.as_deref(), Some("umami"));
    let names: Vec<&str> = t.apps.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["umami", "postgresql"]);
    assert_eq!(t.apps[0].image, "ghcr:umami-software/umami:3.0.3");
    assert_eq!(t.apps[0].depends_on, ["postgresql"]);
    assert_eq!(t.apps[1].image, "docker:postgres:16-alpine");
    assert_eq!(
        t.apps[1].volumes,
        ["postgresql-data:/var/lib/postgresql/data"]
    );
    let vars: Vec<(&str, VarKind)> = t
        .variables
        .iter()
        .map(|v| (v.name.as_str(), v.kind))
        .collect();
    assert_eq!(
        vars,
        [
            ("domain_umami", VarKind::Domain),
            ("user_postgres", VarKind::Username),
            ("password_postgres", VarKind::Password),
            ("postgres_db", VarKind::String),
            ("password_64_umami", VarKind::Password),
        ]
    );
    assert_eq!(var(&t, "postgres_db").default.as_deref(), Some("umami"));
    // The health check's `$$` is the shell's `$`.
    let h = t.apps[1].healthcheck.as_ref().unwrap();
    assert_eq!(
        h["test"][1],
        "pg_isready -U $${POSTGRES_USER} -d $${POSTGRES_DB}"
    );
    assert!(plan_it(&t).is_ok());
}

#[test]
fn uptime_kuma_translates_cleanly() {
    let (t, r) = tr_meta(&meta("uptime-kuma", UPTIME_KUMA, None), UPTIME_KUMA);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    assert_eq!(t.apps.len(), 1);
    assert_eq!(t.apps[0].volumes, ["uptime-kuma-data:/app/data"]);
    assert_eq!(t.apps[0].domains[0]["port"], 3001);
    assert_eq!(t.apps[0].port, Some(3001));
    assert_eq!(t.variables.len(), 1);
    assert!(plan_it(&t).is_ok());
}

#[test]
fn searxng_files_and_variables() {
    let (t, r) = tr_meta(&meta("searxng", SEARXNG, None), SEARXNG);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let web = &t.apps[0];
    let paths: Vec<&str> = web.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        ["/etc/searxng/settings.yml", "/etc/searxng/limiter.toml"]
    );
    assert!(
        web.files[0]
            .content
            .starts_with("# see https://docs.searxng.org")
    );
    assert!(web.files[1].content.contains("link_token = true"));
    assert!(web.volumes.is_empty());
    assert_eq!(web.env["SEARXNG_REDIS_URL"], "redis://${host:redis}:6379/0");
    assert_eq!(web.env["BASE_URL"], "https://${domain_searxng}");
    assert_eq!(var(&t, "instance_name").default.as_deref(), Some("coolify"));
    assert_eq!(var(&t, "password_searxngsecret").length, Some(32));
    assert!(plan_it(&t).is_ok());
}
