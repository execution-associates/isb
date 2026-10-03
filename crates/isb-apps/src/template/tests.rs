use super::*;

fn ctx_with<'a>(ip: Option<IpAddr>, ep: &'a EntrypointFn) -> Context<'a> {
    Context {
        public_ip: ip,
        entrypoint: ep,
        free_port: &|| Some(24242),
    }
}

/// An app as JSON, its env as a map.
fn val(a: &AppSpec) -> Value {
    let mut v = serde_json::to_value(a).unwrap();
    let env: serde_json::Map<String, Value> = a
        .env
        .vars()
        .map(|(k, e)| {
            (
                k.to_string(),
                match e {
                    crate::app::EnvValue::Plain(s) => json!(s),
                    crate::app::EnvValue::Secret { secret } => json!({"secret": secret}),
                },
            )
        })
        .collect();
    v["env"] = Value::Object(env);
    v
}

fn no_entrypoint(_: &str) -> Result<Option<Vec<String>>> {
    Ok(None)
}

fn params(org: &str, values: &[(&str, &str)]) -> Params {
    Params {
        org: OrgId::new(org).unwrap(),
        project: "shop".into(),
        environment: "production".into(),
        instance: "stats".into(),
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

const TWO: &str = r#"
id: stats
name: Stats
description: x
variables:
  - {name: domain, type: domain}
  - {name: db_password, type: password, length: 20}
  - {name: admin_email, type: email}
  - {name: plan, default: free, choices: [free, pro]}
  - {name: url, value: "https://${domain}/"}
  - {name: key, type: hex, bytes: 8}
apps:
  - name: db
    image: docker:postgres:16-alpine
    volumes: ["data:/var/lib/postgresql/data"]
    env: {POSTGRES_PASSWORD: "${db_password}"}
    healthcheck: {test: [CMD-SHELL, "pg_isready -p $$PORT"], interval: 5s}
  - name: web
    image: ghcr:acme/stats:1
    port: 8000
    depends_on: [db]
    env:
      DATABASE_URL: "postgres://u:${db_password}@${host:db}:5432/x"
      BASE_URL: "${url}"
      ADMIN: "${admin_email}"
      PLAN: "${plan}"
      LITERAL: "a $$HOME b"
    command: [serve, "--key=${key}", --plan, "${plan}"]
    files:
      - {path: /etc/stats.conf, content: "url = ${url}\npassword = ${db_password}\n"}
    domains:
      - {host: "${domain}"}
main: web
notes: [hello]
"#;

#[test]
fn expressions_parse() {
    assert_eq!(
        parse_expr("a ${x} $$ ${host:db} $HOME 5$").unwrap(),
        vec![
            Part::Lit("a ".into()),
            Part::Var("x".into()),
            Part::Lit(" $ ".into()),
            Part::Host("db".into()),
            Part::Lit(" $HOME 5$".into()),
        ]
    );
    assert!(parse_expr("${X}").is_err(), "upper case is not a variable");
    assert!(parse_expr("${x:-d}").is_err());
    assert!(parse_expr("${x").is_err());
}

#[test]
fn validation_catches_mistakes() {
    let ok = Template::from_yaml(TWO).unwrap();
    assert_eq!(ok.order().unwrap(), ["db", "web"]);
    let bad = |find: &str, replace: &str| {
        let y = TWO.replace(find, replace);
        Template::from_yaml(&y).expect_err(&format!("{find} -> {replace}"))
    };
    bad("${host:db}", "${host:nope}");
    bad("BASE_URL: \"${url}\"", "BASE_URL: \"${nope}\"");
    bad("depends_on: [db]", "depends_on: [web]");
    bad("main: web", "main: nope");
    bad("id: stats", "id: Stats");
    bad(
        "{name: key, type: hex, bytes: 8}",
        "{name: key, type: hex, value: x}",
    );
    // A cycle between apps and between variables.
    let mut t = ok.clone();
    t.apps[0].depends_on = vec!["web".into()];
    assert!(t.validate().is_err());
    let mut t = ok.clone();
    t.variables.push(Variable {
        name: "a".into(),
        value: Some("${b}".into()),
        ..Default::default()
    });
    t.variables.push(Variable {
        name: "b".into(),
        value: Some("${a}".into()),
        ..Default::default()
    });
    assert!(t.validate().is_err());
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn renders_apps_and_secrets() {
    let t = Template::from_yaml(TWO).unwrap();
    let ep = no_entrypoint;
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    let p = plan(
        &t,
        &params("acme", &[("admin_email", "ops@example.com")]),
        &ctx_with(Some(ip), &ep),
    )
    .unwrap();
    assert_eq!(p.stack, "shop-production");
    assert_eq!(p.order, ["stats-db", "stats"]);
    let db = &p.apps[0];
    let web = &p.apps[1];
    assert_eq!(db.name, "stats-db");
    assert_eq!(web.name, "stats");
    // The password is a secret; the db gets a reference to it.
    let v = val(db);
    assert_eq!(
        v["env"]["POSTGRES_PASSWORD"],
        json!({"secret": "tpl.stats.db_password"})
    );
    assert_eq!(
        v["healthcheck"]["test"],
        json!(["CMD-SHELL", "pg_isready -p $PORT"])
    );
    let w = val(web);
    // A value built from a secret is its own secret.
    assert_eq!(
        w["env"]["DATABASE_URL"],
        json!({"secret": "tpl.stats.web.env.DATABASE_URL"})
    );
    let url_secret = p
        .secrets
        .iter()
        .find(|s| s.name == "tpl.stats.web.env.DATABASE_URL")
        .unwrap();
    assert!(
        url_secret
            .value
            .contains("@stats-db.shop-production:5432/x")
    );
    let host = "stats-shop-production-acme.203-0-113-7.sslip.io";
    assert_eq!(w["env"]["BASE_URL"], json!(format!("https://{host}/")));
    assert_eq!(w["env"]["PLAN"], json!("free"));
    assert_eq!(w["env"]["LITERAL"], json!("a $HOME b"));
    assert_eq!(w["env"]["ADMIN"], json!("ops@example.com"));
    // One app uses the domain: it stays `auto` for the ingress.
    assert_eq!(w["domains"][0]["host"], json!("auto"));
    assert_eq!(p.urls, [format!("https://{host}/")]);
    // A secret in the command line goes through a variable and sh.
    let cmd = w["command"].as_array().unwrap();
    assert_eq!(cmd[0], "/bin/sh");
    assert_eq!(
        cmd[2],
        "exec 'serve' '--key='\"${ISB_TPL_KEY}\" '--plan' 'free'"
    );
    assert_eq!(w["env"]["ISB_TPL_KEY"], json!({"secret": "tpl.stats.key"}));
    // Files are secrets too, readable by any user.
    assert_eq!(
        w["files"][0],
        json!({"path": "/etc/stats.conf", "secret": "tpl.stats.web.file1", "mode": "0444"})
    );
    let f = p
        .secrets
        .iter()
        .find(|s| s.name == "tpl.stats.web.file1")
        .unwrap();
    assert!(
        f.value
            .starts_with(&format!("url = https://{host}/\npassword = "))
    );
    // The plan never shows a secret's value.
    let shown = serde_json::to_string(&p).unwrap();
    for s in &p.secrets {
        assert!(!shown.contains(&s.value), "{} leaked", s.name);
    }
    let pw = p
        .variables
        .iter()
        .find(|v| v.name == "db_password")
        .unwrap();
    assert!(pw.secret && pw.value.is_none() && pw.source == "generated");
    assert_eq!(
        p.secrets
            .iter()
            .find(|s| s.name == "tpl.stats.db_password")
            .unwrap()
            .value
            .len(),
        20
    );
    // The computed url depends on no secret: plain.
    let url = p.variables.iter().find(|v| v.name == "url").unwrap();
    assert!(!url.secret);
    assert!(p.notes.iter().any(|n| n == "hello"));
}

#[test]
fn inputs_are_checked() {
    let t = Template::from_yaml(TWO).unwrap();
    let ep = no_entrypoint;
    let c = ctx_with(None, &ep);
    let err = |vals: &[(&str, &str)]| plan(&t, &params("acme", vals), &c).unwrap_err().to_string();
    assert!(err(&[]).contains("admin_email is required"));
    assert!(err(&[("admin_email", "nope")]).contains("an email address"));
    assert!(err(&[("admin_email", "a@b.co"), ("plan", "gold")]).contains("one of free, pro"));
    assert!(err(&[("admin_email", "a@b.co"), ("url", "x")]).contains("computed"));
    assert!(err(&[("admin_email", "a@b.co"), ("bogus", "x")]).contains("no such variable"));
    assert!(err(&[("admin_email", "a@b.co"), ("domain", "not a host")]).contains("hostname"));
    // No public address: a generated hostname cannot go into env.
    assert!(err(&[("admin_email", "a@b.co")]).contains("no public address"));
    // A given domain works without one.
    let p = plan(
        &t,
        &params(
            "acme",
            &[("admin_email", "a@b.co"), ("domain", "stats.example.com")],
        ),
        &c,
    )
    .unwrap();
    let w = val(&p.apps[1]);
    assert_eq!(w["domains"][0]["host"], "stats.example.com");
    assert_eq!(w["env"]["BASE_URL"], "https://stats.example.com/");
    // Multi-app templates need an org with service names.
    assert!(
        plan(
            &t,
            &params(
                "default",
                &[("admin_email", "a@b.co"), ("domain", "s.example.com")]
            ),
            &c
        )
        .unwrap_err()
        .to_string()
        .contains("default org")
    );
}

#[test]
fn args_follow_the_image_entrypoint() {
    let y = r#"
id: one
name: One
apps:
  - name: app
    image: docker:minio/minio
    args: [server, /data]
"#;
    let t = Template::from_yaml(y).unwrap();
    let ep = |i: &str| -> Result<Option<Vec<String>>> {
        assert_eq!(i, "docker:minio/minio");
        Ok(Some(vec!["/usr/bin/docker-entrypoint.sh".into()]))
    };
    let mut p = params("acme", &[]);
    p.instance = "one".into();
    let pl = plan(&t, &p, &ctx_with(None, &ep)).unwrap();
    assert_eq!(pl.apps[0].name, "one");
    assert_eq!(
        pl.apps[0].command,
        Some(json!(["/usr/bin/docker-entrypoint.sh", "server", "/data"]))
    );
    let failing = |_: &str| -> Result<Option<Vec<String>>> { Err(Error::invalid("no skopeo")) };
    assert!(
        plan(&t, &p, &ctx_with(None, &failing))
            .unwrap_err()
            .to_string()
            .contains("entrypoint")
    );
}

#[test]
fn generators_and_jwt() {
    let y = r#"
id: gen
name: Gen
variables:
  - {name: jwt_secret, type: password, length: 40}
  - {name: payload, value: '{"role": "anon", "exp": 1893456000}'}
  - {name: anon_key, type: jwt, jwt: {secret: jwt_secret, payload: "${payload}"}}
  - {name: id, type: uuid}
  - {name: port, type: port}
  - {name: at, type: timestamp, at: "2030-01-01T00:00:00Z", unit: ms}
  - {name: b, type: base64, bytes: 16}
  - {name: u, type: username}
apps:
  - name: app
    image: docker:x
    env:
      A: "${anon_key}"
      ID: "${id}"
      PORT: "${port}"
      AT: "${at}"
      U: "${u}"
"#;
    let t = Template::from_yaml(y).unwrap();
    let ep = no_entrypoint;
    let mut p = params("acme", &[]);
    p.instance = "gen".into();
    let pl = plan(&t, &p, &ctx_with(None, &ep)).unwrap();
    let v = val(&pl.apps[0]);
    assert_eq!(v["env"]["A"], json!({"secret": "tpl.gen.anon_key"}));
    assert_eq!(v["env"]["PORT"], "24242");
    assert_eq!(v["env"]["AT"], "1893456000000");
    assert_eq!(v["env"]["ID"].as_str().unwrap().len(), 36);
    let secret = &pl
        .secrets
        .iter()
        .find(|s| s.name == "tpl.gen.jwt_secret")
        .unwrap()
        .value;
    let jwt = &pl
        .secrets
        .iter()
        .find(|s| s.name == "tpl.gen.anon_key")
        .unwrap()
        .value;
    let parts: Vec<&str> = jwt.split('.').collect();
    use base64::Engine;
    let body: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["role"], "anon");
    assert_eq!(body["exp"], 1893456000);
    assert!(body["iat"].is_number(), "a partial payload gets iat");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .unwrap();
    ring::hmac::verify(&key, format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
}

#[test]
fn shared_generated_domain_is_written_out() {
    let y = r#"
id: split
name: Split
variables:
  - {name: domain, type: domain}
apps:
  - {name: front, image: docker:a, port: 80, domains: [{host: "${domain}"}]}
  - {name: api, image: docker:b, port: 81, domains: [{host: "${domain}", path: /api}]}
main: front
"#;
    let t = Template::from_yaml(y).unwrap();
    let ep = no_entrypoint;
    let mut p = params("acme", &[]);
    p.instance = "split".into();
    let pl = plan(
        &t,
        &p,
        &ctx_with(Some("198.51.100.1".parse().unwrap()), &ep),
    )
    .unwrap();
    let h = "split-shop-production-acme.198-51-100-1.sslip.io";
    for a in &pl.apps {
        assert_eq!(a.domains[0]["host"], json!(h), "{}", a.name);
    }
    assert_eq!(
        pl.urls,
        [format!("https://{h}/"), format!("https://{h}/api")]
    );
}

#[test]
fn every_builtin_plans() {
    let ep = |_: &str| -> Result<Option<Vec<String>>> { Ok(Some(vec!["/entry".into()])) };
    for t in catalog::builtin() {
        let mut p = params("acme", &[]);
        p.instance =
            t.id.chars()
                .take(20)
                .collect::<String>()
                .trim_end_matches('-')
                .to_string();
        let pl = plan(&t, &p, &ctx_with(Some("203.0.113.7".parse().unwrap()), &ep))
            .unwrap_or_else(|e| panic!("{}: {e}", t.id));
        assert_eq!(pl.apps.len(), t.apps.len());
        assert!(!pl.urls.is_empty(), "{}: no URL", t.id);
        // No secret value in a stored app definition.
        let defs = serde_json::to_string(&pl.apps).unwrap();
        for s in &pl.secrets {
            assert!(
                !defs.contains(&s.value),
                "{}: {} in a definition",
                t.id,
                s.name
            );
        }
    }
}
