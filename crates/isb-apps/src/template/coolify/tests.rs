use super::*;
use crate::org::OrgId;
use crate::template::shared::Status;
use crate::template::{Context, Params, VarKind, plan};

// The three fixtures below are templates from coollabsio/coolify
// (templates/compose/{umami,uptime-kuma,searxng}.yaml), copied here as test
// data (searxng's inline files shortened). Coolify is Copyright 2025 Andras
// Bacsai, licensed under the Apache License, Version 2.0
// (https://www.apache.org/licenses/LICENSE-2.0). They are not part of isb's
// shipped code: a catalog fetches the real ones at runtime.

pub(super) const UMAMI: &str = r#"# documentation: https://umami.is
# slogan: Umami is web analytics platform which provides insights into visitor behavior without compromising user privacy.
# category: analytics
# tags: analytics, insights, privacy
# logo: svgs/umami.svg
# port: 3000

services:
  umami:
    image: ghcr.io/umami-software/umami:3.0.3
    environment:
      - SERVICE_URL_UMAMI_3000
      - DATABASE_URL=postgres://$SERVICE_USER_POSTGRES:$SERVICE_PASSWORD_POSTGRES@postgresql:5432/$POSTGRES_DB
      - DATABASE_TYPE=postgres
      - APP_SECRET=$SERVICE_PASSWORD_64_UMAMI
    depends_on:
      postgresql:
        condition: service_healthy
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:3000/api/heartbeat"]
      interval: 5s
      timeout: 20s
      retries: 10
  postgresql:
    image: postgres:16-alpine
    volumes:
      - postgresql-data:/var/lib/postgresql/data
    environment:
      - POSTGRES_USER=$SERVICE_USER_POSTGRES
      - POSTGRES_PASSWORD=$SERVICE_PASSWORD_POSTGRES
      - POSTGRES_DB=${POSTGRES_DB:-umami}
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $${POSTGRES_USER} -d $${POSTGRES_DB}"]
      interval: 5s
      timeout: 20s
      retries: 10
"#;

pub(super) const UPTIME_KUMA: &str = r#"# documentation: https://github.com/louislam/uptime-kuma?tab=readme-ov-file
# slogan: Uptime Kuma is a monitoring tool for tracking the status and performance of your applications in real-time.
# category: monitoring
# tags: monitoring, status, performance, web, services, applications, real-time
# logo: svgs/uptime-kuma.svg
# port: 3001

services:
  uptime-kuma:
    image: louislam/uptime-kuma:2
    environment:
      - SERVICE_URL_UPTIMEKUMA_3001
    volumes:
      - uptime-kuma-data:/app/data
    healthcheck:
      test: ["CMD-SHELL", "extra/healthcheck"]
      interval: 5s
      timeout: 5s
      retries: 10
"#;

pub(super) const SEARXNG: &str = r#"# documentation: https://docs.searxng.org
# slogan: SearXNG is a free internet metasearch engine which aggregates results from more than 70 search services.
# category: search
# tags: search, google, engine, images, documents, rss, proxy, news, web, api
# logo: svgs/searxng.svg
# port: 8080

services:
  searxng:
    image: searxng/searxng
    depends_on:
      redis:
        condition: service_healthy
    environment:
      - SERVICE_URL_SEARXNG_8080
      - INSTANCE_NAME=${INSTANCE_NAME:-coolify}
      - BASE_URL=${SERVICE_URL_SEARXNG_8080}
      - SEARXNG_URL=${SERVICE_URL_SEARXNG_8080}
      - SEARXNG_BIND_ADDRESS=${SEARXNG_BIND_ADDRESS:-0.0.0.0}
      - SEARXNG_SECRET=${SERVICE_PASSWORD_SEARXNGSECRET}
      - SEARXNG_REDIS_URL=redis://redis:6379/0
    healthcheck:
      test:
        - CMD
        - wget
        - "-q"
        - "--spider"
        - "http://127.0.0.1:8080/healthz"
      interval: 5s
      timeout: 5s
      retries: 3
    volumes:
      - type: bind
        source: ./settings.yml
        target: /etc/searxng/settings.yml
        content: |
          # see https://docs.searxng.org/admin/settings/settings.html#settings-use-default-settings
          use_default_settings: true
          server:
            limiter: false
            image_proxy: true
      - type: bind
        source: ./limiter.toml
        target: /etc/searxng/limiter.toml
        content: |
          [botdetection.ip_limit]
          link_token = true

  redis:
    image: "redis:7"
    restart: always
    volumes:
      - "redis-data:/data"
    healthcheck:
      test:
        - CMD
        - redis-cli
        - ping
      interval: 5s
      timeout: 5s
      retries: 3
"#;

pub(super) fn tr(compose: &str) -> (Option<Template>, Report) {
    let m = Meta {
        id: "t".into(),
        name: "T".into(),
        ..Default::default()
    };
    translate(&m, compose)
}

/// A translation that deploys: the template and its report.
pub(super) fn ok(compose: &str) -> (Template, Report) {
    let (t, r) = tr(compose);
    (t.unwrap_or_else(|| panic!("refused: {r:?}")), r)
}

pub(super) fn tr_meta(m: &Meta, compose: &str) -> (Template, Report) {
    let (t, r) = translate(m, compose);
    (t.unwrap_or_else(|| panic!("refused: {r:?}")), r)
}

pub(super) fn refusals(compose: &str) -> Vec<String> {
    let (t, r) = tr(compose);
    assert!(t.is_none(), "should be refused: {r:?}");
    assert_eq!(r.status, Status::Refused);
    r.refusals
}

/// One service `a` with `extra` lines (indented four spaces) and a restart
/// policy.
pub(super) fn svc(extra: &str) -> String {
    format!("services:\n  a:\n    image: a:1\n    restart: always\n{extra}")
}

pub(super) fn var<'a>(t: &'a Template, name: &str) -> &'a crate::template::Variable {
    t.var(name)
        .unwrap_or_else(|| panic!("no variable {name}: {:?}", t.variables))
}

/// Plan a deploy of a translation, giving every required input a value.
pub(super) fn plan_it(t: &Template) -> Result<(), String> {
    let values = t
        .variables
        .iter()
        .filter(|v| v.required == Some(true))
        .map(|v| (v.name.clone(), "x".to_string()))
        .collect();
    let p = Params {
        org: OrgId::new("acme").unwrap(),
        project: "shop".into(),
        environment: "production".into(),
        instance: "inst".into(),
        values,
    };
    let ep = |_: &str| Ok(Some(vec![]));
    let ctx = Context {
        public_ip: Some("203.0.113.7".parse().unwrap()),
        entrypoint: &ep,
        free_port: &|| Some(24242),
    };
    plan(t, &p, &ctx).map(|_| ()).map_err(|e| e.to_string())
}

/// Every translation of a directory of Coolify templates, as JSON lines to
/// `COOLIFY_SURVEY_OUT`: `COOLIFY_TEMPLATES=<dir of *.yaml> cargo test
/// -p isb-apps coolify::tests::survey -- --ignored`. Each deployable one is
/// also planned. `COOLIFY_DUMP=id,id` prints those translations.
#[test]
#[ignore = "reads a checkout of coollabsio/coolify"]
fn survey() {
    let dir = std::env::var("COOLIFY_TEMPLATES").expect("COOLIFY_TEMPLATES");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".yaml") || n.ends_with(".yml"))
        .collect();
    names.sort();
    let mut out = String::new();
    for n in names {
        let id = n.trim_end_matches(".yaml").trim_end_matches(".yml");
        let text = std::fs::read_to_string(format!("{dir}/{n}")).unwrap();
        let m = meta(id, &text, None);
        let (t, r) = translate(&m, &text);
        if std::env::var("COOLIFY_DUMP").is_ok_and(|d| d.split(',').any(|x| x == id)) {
            let y = t.as_ref().map(|t| serde_yaml_ng::to_string(t).unwrap());
            eprintln!("=== {id}\n{}\n{r:#?}", y.unwrap_or_default());
        }
        let planned = t.as_ref().map(plan_it).and_then(Result::err);
        let line = serde_json::json!({
            "plan_error": planned,
            "id": id,
            "ignore": m.ignore,
            "status": r.status,
            "notes": r.notes,
            "refusals": r.refusals,
            "apps": t.as_ref().map_or(0, |t| t.apps.len()),
        });
        out.push_str(&format!("{line}\n"));
    }
    std::fs::write(
        std::env::var("COOLIFY_SURVEY_OUT").expect("COOLIFY_SURVEY_OUT"),
        out,
    )
    .unwrap();
}

// --- headers ----------------------------------------------------------------

#[test]
fn header_comments_are_read_until_the_compose_starts() {
    let h = header(UMAMI);
    let keys: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "documentation",
            "slogan",
            "category",
            "tags",
            "logo",
            "port"
        ]
    );
    assert_eq!(h[0].1, "https://umami.is", "the value keeps its colons");
    // Comments inside the compose are not metadata.
    let h = header("# slogan: a\nservices: {}\n# port: 9\n");
    assert_eq!(h.len(), 1);
    // Comments that are not `key: value` are skipped.
    let h = header("# just a comment\n#   \n# tags: a, b\n\nservices: {}");
    assert_eq!(h, [("tags".to_string(), "a, b".to_string())]);
}

#[test]
fn metadata_from_the_header() {
    let m = meta(
        "uptime-kuma",
        UPTIME_KUMA,
        Some("https://raw.githubusercontent.com/coollabsio/coolify/main/"),
    );
    assert_eq!(m.id, "uptime-kuma");
    assert_eq!(m.name, "Uptime Kuma");
    assert!(
        m.description
            .starts_with("Uptime Kuma is a monitoring tool")
    );
    assert_eq!(m.category, "monitoring");
    // The category comes first, then the tags, once each.
    assert_eq!(m.tags[..3], ["monitoring", "status", "performance"]);
    assert_eq!(m.tags.iter().filter(|t| *t == "monitoring").count(), 1);
    assert_eq!(
        m.logo.as_deref(),
        Some(
            "https://raw.githubusercontent.com/coollabsio/coolify/main/public/svgs/uptime-kuma.svg"
        )
    );
    assert_eq!(
        m.docs.as_deref(),
        Some("https://github.com/louislam/uptime-kuma?tab=readme-ov-file")
    );
    assert_eq!(m.port, Some(3001));
    assert!(!m.ignore);
    // No base, no logo (a checkout has none to serve).
    assert_eq!(meta("x", UPTIME_KUMA, None).logo, None);
}

#[test]
fn metadata_is_cleaned_and_checked() {
    let m = meta(
        "x",
        "# slogan: \"Quoted slogan.\"\n# documentation: javascript:alert(1)\n# logo: ../../etc/passwd\n# port: nope\n# ignore: true\n# tags: A, b ,, a\n",
        Some("https://example.com"),
    );
    assert_eq!(m.description, "Quoted slogan.");
    assert_eq!(m.docs, None, "only http(s) links");
    assert_eq!(m.logo, None, "a logo path stays inside public/");
    assert_eq!(m.port, None);
    assert!(m.ignore);
    assert_eq!(m.tags, ["a", "b"]);
    for bad in ["/abs.svg", "svgs/a b.svg", "svgs/a?x=1", ".hidden"] {
        let m = meta("x", &format!("# logo: {bad}\n"), Some("https://e.com"));
        assert_eq!(m.logo, None, "{bad}");
    }
}

#[test]
fn names_are_titled_from_the_file_name() {
    assert_eq!(titled("umami"), "Umami");
    assert_eq!(titled("uptime-kuma-with-mysql"), "Uptime Kuma With Mysql");
    assert_eq!(titled("a_b.c"), "A B C");
    assert_eq!(titled("--"), "");
}

// --- magic names ------------------------------------------------------------

fn generator(token: &str) -> Gen {
    match classify(token) {
        Some(Magic::Gen(g)) => g,
        other => panic!("{token}: {other:?}"),
    }
}

#[test]
fn every_magic_name_is_classified() {
    let url = |n: &str, p: Option<u16>| {
        Some(Magic::Url {
            name: n.into(),
            port: p,
        })
    };
    let fqdn = |n: &str, p: Option<u16>| {
        Some(Magic::Fqdn {
            name: n.into(),
            port: p,
        })
    };
    assert_eq!(classify("SERVICE_URL_UMAMI"), url("UMAMI", None));
    assert_eq!(classify("SERVICE_URL_UMAMI_3000"), url("UMAMI", Some(3000)));
    assert_eq!(classify("SERVICE_URL_MY_APP_80"), url("MY_APP", Some(80)));
    assert_eq!(classify("SERVICE_URL_S3_8333"), url("S3", Some(8333)));
    assert_eq!(classify("SERVICE_URL_X_70000"), url("X_70000", None));
    assert_eq!(classify("SERVICE_FQDN_UMAMI"), fqdn("UMAMI", None));
    assert_eq!(
        classify("SERVICE_FQDN_UMAMI_3000"),
        fqdn("UMAMI", Some(3000))
    );
    assert_eq!(
        classify("SERVICE_NAME_POSTGRES"),
        Some(Magic::Name("POSTGRES".into()))
    );
}

#[test]
fn every_generated_name_is_classified() {
    assert_eq!(generator("SERVICE_USER_DB"), Gen::User);
    assert_eq!(generator("SERVICE_LOWERCASEUSER_DB"), Gen::User);
    let pw = |len, symbols| Gen::Password { len, symbols };
    assert_eq!(generator("SERVICE_PASSWORD_DB"), pw(32, false));
    assert_eq!(generator("SERVICE_PASSWORD_64_DB"), pw(64, false));
    assert_eq!(generator("SERVICE_PASSWORDWITHSYMBOLS_DB"), pw(32, true));
    assert_eq!(generator("SERVICE_PASSWORDWITHSYMBOLS_64_DB"), pw(64, true));
    assert_eq!(generator("SERVICE_BASE64_K"), Gen::Alnum(32));
    assert_eq!(generator("SERVICE_BASE64_32_K"), Gen::Alnum(32));
    assert_eq!(generator("SERVICE_BASE64_64_K"), Gen::Alnum(64));
    assert_eq!(generator("SERVICE_BASE64_128_K"), Gen::Alnum(128));
    assert_eq!(generator("SERVICE_REALBASE64_K"), Gen::RealBase64(32));
    assert_eq!(generator("SERVICE_REALBASE64_32_K"), Gen::RealBase64(32));
    assert_eq!(generator("SERVICE_REALBASE64_64_K"), Gen::RealBase64(64));
    assert_eq!(generator("SERVICE_REALBASE64_128_K"), Gen::RealBase64(128));
    assert_eq!(generator("SERVICE_HEX_32_K"), Gen::Hex(32));
    assert_eq!(generator("SERVICE_HEX_64_K"), Gen::Hex(64));
    assert_eq!(generator("SERVICE_HEX_128_K"), Gen::Hex(128));
    assert_eq!(generator("SERVICE_SUPABASEANON_K"), Gen::Jwt("anon"));
    assert_eq!(
        generator("SERVICE_SUPABASESERVICE_K"),
        Gen::Jwt("service_role")
    );
}

#[test]
fn other_names_are_ordinary_variables() {
    for t in [
        "SERVICE_KEY",
        "SERVICE_ROLE_KEY",
        "SERVICE_PASSWORD",
        "SERVICE_URL",
        "SERVICE_HEX_K",
        "SERVICE_HEX_32",
        "SERVICE_PASSWORD_x",
        "SERVICE_FOO_BAR",
        "MY_SERVICE_URL_X",
        "service_url_x",
    ] {
        assert_eq!(classify(t), None, "{t}");
    }
}

#[test]
fn generated_values_become_generated_variables() {
    let (t, r) = ok(&svc("    environment:
      - U=$SERVICE_USER_A
      - LU=${SERVICE_LOWERCASEUSER_A}
      - P=$SERVICE_PASSWORD_A
      - P64=$SERVICE_PASSWORD_64_A
      - B=$SERVICE_BASE64_A
      - B64=$SERVICE_BASE64_64_A
      - B128=$SERVICE_BASE64_128_A
      - RB=$SERVICE_REALBASE64_A
      - RB64=$SERVICE_REALBASE64_64_A
      - H32=$SERVICE_HEX_32_A
      - H64=$SERVICE_HEX_64_A
      - SAME=$SERVICE_PASSWORD_A
"));
    assert_eq!(r.status, Status::Clean, "{r:?}");
    let v = |n: &str| var(&t, n);
    assert_eq!(
        (v("user_a").kind, v("user_a").length),
        (VarKind::Username, Some(16))
    );
    assert_eq!(v("lowercaseuser_a").kind, VarKind::Username);
    assert_eq!(
        (v("password_a").kind, v("password_a").length),
        (VarKind::Password, Some(32))
    );
    assert_eq!(v("password_64_a").length, Some(64));
    assert_eq!(
        (v("base64_a").kind, v("base64_a").length),
        (VarKind::Password, Some(32))
    );
    assert_eq!(v("base64_64_a").length, Some(64));
    assert_eq!(v("base64_128_a").length, Some(128));
    assert_eq!(
        (v("realbase64_a").kind, v("realbase64_a").bytes),
        (VarKind::Base64, Some(32))
    );
    assert_eq!(v("realbase64_64_a").bytes, Some(64));
    // HEX_<n> is n characters: n/2 bytes.
    assert_eq!(
        (v("hex_32_a").kind, v("hex_32_a").bytes),
        (VarKind::Hex, Some(16))
    );
    assert_eq!(v("hex_64_a").bytes, Some(32));
    let env = &t.apps[0].env;
    assert_eq!(env["U"], "${user_a}");
    assert_eq!(env["P64"], "${password_64_a}");
    assert_eq!(env["SAME"], env["P"], "one name, one value");
    assert_eq!(t.variables.len(), 11, "one variable per name");
    assert!(plan_it(&t).is_ok());
}

#[test]
fn passwords_with_symbols_have_none() {
    let (t, r) = ok(&svc(
        "    environment:\n      - P=$SERVICE_PASSWORDWITHSYMBOLS_A\n",
    ));
    assert_eq!(r.status, Status::Notes);
    assert!(r.notes[0].contains("no symbols"), "{r:?}");
    assert_eq!(var(&t, "passwordwithsymbols_a").kind, VarKind::Password);
}

#[test]
fn supabase_jwts_are_signed_with_the_jwt_password() {
    let (t, _) = ok(&svc(
        "    environment:\n      - ANON=$SERVICE_SUPABASEANON_KEY\n      - SERVICE=${SERVICE_SUPABASESERVICE_KEY}\n",
    ));
    let anon = var(&t, "supabaseanon_key");
    assert_eq!(anon.kind, VarKind::Jwt);
    let j = anon.jwt.as_ref().unwrap();
    assert_eq!(j.secret, "password_jwt");
    assert!(j.payload.as_ref().unwrap().contains(r#""role":"anon""#));
    let svc = var(&t, "supabaseservice_key");
    assert!(
        svc.jwt
            .as_ref()
            .unwrap()
            .payload
            .as_ref()
            .unwrap()
            .contains("service_role")
    );
    // The signing secret is made even when the template does not say so.
    assert_eq!(var(&t, "password_jwt").kind, VarKind::Password);
    assert!(plan_it(&t).is_ok());
}

#[test]
fn service_names_are_the_apps_names() {
    let (t, _) = ok(r#"
services:
  my-db:
    image: pg:1
    restart: always
  app:
    image: app:1
    restart: always
    environment:
      - DB=$SERVICE_NAME_MY_DB
      - DB2=${SERVICE_NAME_MY_DB}:5432
"#);
    assert_eq!(t.apps[1].env["DB"], "${host:my-db}");
    assert_eq!(t.apps[1].env["DB2"], "${host:my-db}:5432");
    let r = refusals(&svc("    environment:\n      - X=$SERVICE_NAME_NOPE\n"));
    assert!(r[0].contains("no service named NOPE"), "{r:?}");
}

// --- domains ----------------------------------------------------------------

pub(super) const UMAMI_LIKE: &str = r#"
services:
  umami:
    image: umami:1
    restart: always
    environment:
      - SERVICE_URL_UMAMI_3000
      - PUBLIC=$SERVICE_URL_UMAMI_3000
      - HOST_ONLY=${SERVICE_FQDN_UMAMI_3000}
  worker:
    image: worker:1
    restart: always
"#;

#[test]
fn a_service_url_with_a_port_is_a_domain_on_that_port() {
    let (t, r) = ok(UMAMI_LIKE);
    assert_eq!(r.status, Status::Clean, "{r:?}");
    assert_eq!(var(&t, "domain_umami").kind, VarKind::Domain);
    let app = &t.apps[0];
    assert_eq!(app.port, Some(3000));
    assert_eq!(app.domains.len(), 1);
    assert_eq!(app.domains[0]["host"], "${domain_umami}");
    assert_eq!(app.domains[0]["port"], 3000);
    assert_eq!(app.env["SERVICE_URL_UMAMI_3000"], "https://${domain_umami}");
    assert_eq!(
        app.env["PUBLIC"], "https://${domain_umami}",
        "the URL as a value"
    );
    assert_eq!(
        app.env["HOST_ONLY"], "${domain_umami}",
        "the FQDN as a value"
    );
    assert_eq!(t.main.as_deref(), Some("umami"));
    assert!(t.apps[1].domains.is_empty());
    assert!(plan_it(&t).is_ok());
}

#[test]
fn a_domain_names_the_service_that_declares_it() {
    // The name is a label: the declaring service gets the domain.
    let (t, _) = ok(r#"
services:
  web:
    image: web:1
    restart: always
    expose: [8080]
    environment:
      - SERVICE_FQDN_SHOP
      - URL=$SERVICE_URL_SHOP
  api:
    image: api:1
    restart: always
    environment:
      - SERVICE_URL_SHOP_9000=/api
      - SHOP_URL=$SERVICE_URL_SHOP
"#);
    let web = &t.apps[0];
    assert_eq!(
        web.domains[0]["port"], 8080,
        "no port: what the service exposes"
    );
    assert_eq!(web.env["SERVICE_FQDN_SHOP"], "${domain_shop}");
    let api = &t.apps[1];
    assert_eq!(
        api.domains[0]["host"], "${domain_shop}",
        "one name, one host"
    );
    assert_eq!(api.domains[0]["port"], 9000);
    assert_eq!(api.domains[0]["path"], "/api");
    assert_eq!(
        api.env["SERVICE_URL_SHOP_9000"],
        "https://${domain_shop}/api"
    );
    assert_eq!(
        t.variables
            .iter()
            .filter(|v| v.kind == VarKind::Domain)
            .count(),
        1
    );
    assert_eq!(t.main.as_deref(), Some("web"));
}

#[test]
fn a_domain_without_a_port_takes_the_header_port_or_80() {
    let compose = svc("    environment:\n      - SERVICE_URL_WEB\n");
    let m = Meta {
        id: "t".into(),
        name: "T".into(),
        port: Some(4000),
        ..Default::default()
    };
    let (t, r) = tr_meta(&m, &compose);
    assert_eq!(t.apps[0].domains[0]["port"], 4000);
    assert_eq!(r.status, Status::Clean);
    let (t, r) = ok(&compose);
    assert_eq!(t.apps[0].domains[0]["port"], 80);
    assert!(r.notes[0].contains("80 is assumed"), "{r:?}");
    // A published container port is the last guess.
    let (t, _) = ok(&svc(
        "    ports: ['127.0.0.1:9:7000']\n    environment:\n      - SERVICE_URL_WEB\n",
    ));
    assert_eq!(t.apps[0].domains[0]["port"], 7000);
}

#[test]
fn a_name_given_with_and_without_a_port_is_one_domain() {
    let (t, _) = ok(&svc(
        "    environment:\n      - SERVICE_URL_WEB_8000\n      - SERVICE_URL_WEB\n      - SERVICE_FQDN_WEB_8000\n",
    ));
    assert_eq!(t.apps[0].domains.len(), 1, "{:?}", t.apps[0].domains);
    assert_eq!(t.apps[0].domains[0]["port"], 8000);
}

#[test]
fn a_name_nothing_declares_goes_to_the_service_of_that_name() {
    let (t, _) = ok(r#"
services:
  my-app:
    image: app:1
    restart: always
    expose: [3000]
    environment:
      - BASE=$SERVICE_URL_MY_APP
"#);
    assert_eq!(t.apps[0].domains[0]["port"], 3000);
    assert_eq!(t.apps[0].env["BASE"], "https://${domain_my_app}");
    let r = refusals(&svc("    environment:\n      - BASE=$SERVICE_URL_OTHER\n"));
    assert!(
        r[0].contains("no service declares or is named OTHER"),
        "{r:?}"
    );
}

#[test]
fn a_url_given_as_another_value_is_not_a_declaration() {
    let (t, _) = ok(r#"
services:
  web:
    image: web:1
    restart: always
    environment:
      - SERVICE_URL_WEB_80
  other:
    image: o:1
    restart: always
    environment:
      SERVICE_URL_WEB_80: ${SERVICE_URL_WEB_80}
"#);
    assert!(t.apps[1].domains.is_empty());
    assert_eq!(t.apps[1].env["SERVICE_URL_WEB_80"], "https://${domain_web}");
}
