use super::*;
use std::collections::HashMap;

fn load_with(docs: &[&str], env: &[(&str, &str)]) -> Result<Project> {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let docs: Vec<(PathBuf, String)> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| (PathBuf::from(format!("f{i}.yaml")), d.to_string()))
        .collect();
    load_docs(&docs, Path::new("/tmp/My Project"), None, &|k| {
        env.get(k).cloned()
    })
}

#[test]
fn file_secret_variables_are_checked_and_warned_about() {
    let doc = "secrets: {db: {external: true}, t: {external: true}}\nservices:\n  pg: {image: docker:postgres, environment: {POSTGRES_PASSWORD: {secret: db, as: file}, TOKEN: {secret: t}}}\n  sys: {image: images:debian/12, command: [x], environment: {TOKEN: {secret: t}}}\n";
    let p = load_with(&[doc], &[]).unwrap();
    // Only the OCI image's variable is instance config.
    let w = crate::stack::secrets::env_exposure_warning(&p.file).unwrap();
    assert!(
        w.starts_with("1 secret variable (pg.TOKEN) is plain text"),
        "{w}"
    );
    let all_files = doc.replace(
        "TOKEN: {secret: t}}}\n  sys",
        "TOKEN: {secret: t, as: file}}}\n  sys",
    );
    let p = load_with(&[&all_files], &[]).unwrap();
    assert_eq!(crate::stack::secrets::env_exposure_warning(&p.file), None);
    // Undeclared, and a KEY_FILE set twice.
    let e = load_with(
        &[&doc.replace("{secret: db, as: file}", "{secret: nope, as: file}")],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("not declared"), "{e}");
    let twice = doc.replace(
        "TOKEN: {secret: t}}}\n  sys",
        "TOKEN: {secret: t}, POSTGRES_PASSWORD_FILE: /x}}\n  sys",
    );
    let e = load_with(&[&twice], &[]).unwrap_err().to_string();
    assert!(e.contains("POSTGRES_PASSWORD_FILE is set"), "{e}");
}

#[test]
fn secret_sources_are_validated() {
    let svc = "services:\n  web: {image: x, secrets: [k]}\n";
    for ok in [
        "{file: ./k}",
        "{environment: K}",
        "{external: true}",
        "{external: true, name: db.password}",
        "{age: \"YWdl\"}",
        "{driver: onepassword, name: \"op://vault/item/field\"}",
    ] {
        let doc = format!("secrets:\n  k: {ok}\n{svc}");
        assert!(load_with(&[&doc], &[]).is_ok(), "{ok}");
    }
    for (bad, why) in [
        ("{}", "exactly one"),
        ("{file: ./k, environment: K}", "exactly one"),
        ("{external: true, age: x}", "exactly one"),
        ("{driver: onepassword}", "driver needs name"),
        ("{environment: K, name: x}", "name goes with"),
        ("{external: true, name: \"a/b\"}", "secret name"),
        ("{age: \"  \"}", "age is empty"),
        ("{vault: x}", "unknown field"),
    ] {
        let doc = format!("secrets:\n  k: {bad}\n{svc}");
        let e = load_with(&[&doc], &[]).unwrap_err().to_string();
        assert!(e.contains(why), "{bad}: {e}");
    }
    // An external secret's key is its store name unless `name` says.
    let bad = "secrets:\n  k/x: {external: true}\nservices:\n  web: {image: x, secrets: [k/x]}\n";
    assert!(load_with(&[bad], &[]).is_err());
    let p = load_with(&[&format!("secrets:\n  k: {{external: true}}\n{svc}")], &[]).unwrap();
    assert_eq!(p.file.secrets["k"].store_name("k"), Some("k"));
    // Read from the org's store by the caller; missing, it says so.
    let e = p.secret_values().unwrap_err().to_string();
    assert!(e.contains("external"), "{e}");
    assert_eq!(p.store_backed_secrets().len(), 1);
    let mut p2 = p.clone();
    p2.store_secrets.0.insert("k".into(), b"v".to_vec());
    assert_eq!(p2.secret_values().unwrap()["k"], b"v");
    assert!(
        !format!("{p2:?}").contains("118"),
        "values are not in Debug"
    );
}

#[test]
fn environment_secrets() {
    let mut ok = load_with(
            &["secrets: {k: {environment: K}}\nservices:\n  web: {image: docker:busybox, environment: {TOKEN: {secret: k}, A: 1}}\n"],
            &[],
        )
        .unwrap();
    ok.vars.insert("K".into(), "v".into());
    let web = &ok.file.services["web"];
    assert_eq!(web.env.secrets["TOKEN"], "k");
    assert_eq!(web.env["A"], "1");
    assert_eq!(ok.secret_values().unwrap()["k"], b"v");
    // An undeclared secret.
    let e = load_with(
        &["services:\n  web: {image: docker:busybox, environment: {T: {secret: nope}}}\n"],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("not declared"), "{e}");
    // A system image needs a command to hand the variable to.
    let e = load_with(
            &["secrets: {k: {environment: K}}\nservices:\n  web: {image: dev-base, environment: {T: {secret: k}}}\n"],
            &[("K", "v")],
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("need a command"), "{e}");
    assert!(
            load_with(
                &["secrets: {k: {environment: K}}\nservices:\n  web: {image: dev-base, command: [app], environment: {T: {secret: k}}}\n"],
                &[("K", "v")],
            )
            .is_ok()
        );
    // refresh goes with a driver, and is at least 10s.
    let svc = "services:\n  web: {image: x, secrets: [k]}\n";
    for (bad, why) in [
        ("{external: true, refresh: 1h}", "refresh goes with driver"),
        ("{driver: d, name: r, refresh: 1s}", "at least 10s"),
        ("{driver: d, name: r, refresh: soon}", "refresh"),
    ] {
        let doc = format!("secrets:\n  k: {bad}\n{svc}");
        let e = load_with(&[&doc], &[]).unwrap_err().to_string();
        assert!(e.contains(why), "{bad}: {e}");
    }
    let p = load_with(
        &[&format!(
            "secrets:\n  k: {{driver: d, name: r, refresh: 30m}}\n{svc}"
        )],
        &[],
    )
    .unwrap();
    assert_eq!(
        p.file.secrets["k"].refresh_interval(),
        std::time::Duration::from_secs(1800)
    );
}

#[test]
fn defaults_names_from_project() {
    let p = load_with(&["services:\n  web: {image: dev-base}\n"], &[]).unwrap();
    assert_eq!(p.name, "my-project");
    assert_eq!(
        p.file.services["web"].name.as_deref(),
        Some("my-project-web")
    );
    let p = load_with(&["name: lasso\nservices:\n  Web_1: {image: x}\n"], &[]).unwrap();
    assert_eq!(
        p.file.services["Web_1"].name.as_deref(),
        Some("lasso-web-1")
    );
}

#[test]
fn named_volumes_are_project_prefixed() {
    let p = load_with(
            &["name: app\nvolumes:\n  cache: {}\n  shared: {external: true}\n  pinned: {name: exactly-this}\nservices:\n  web:\n    image: x\n    volumes: [cache:/c, shared:/s, pinned:/p]\n"],
            &[],
        )
        .unwrap();
    let v = &p.file.volumes;
    assert_eq!(v["cache"].name.as_deref(), Some("app_cache"));
    assert_eq!(v["shared"].name.as_deref(), Some("shared"));
    assert_eq!(v["pinned"].name.as_deref(), Some("exactly-this"));
    // Mounts keep the key; resolution maps it to the volume's name.
    assert_eq!(p.file.services["web"].volumes[0].source, "cache");
}

#[test]
fn undeclared_named_volume_is_an_error() {
    let e = load_with(
        &["services:\n  web: {image: x, volumes: [cache:/c]}\n"],
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("\"cache\"") && e.contains("not declared"), "{e}");
}

#[test]
fn interpolates_and_types() {
    let p = load_with(
            &["services:\n  web:\n    container_name: \"${NAME}\"\n    image: dev-base\n    cpus: ${CPUS:-8}\n    labels: {wt: \"${WT}\"}\n"],
            &[("NAME", "dev-x"), ("WT", "/w")],
        )
        .unwrap();
    let w = &p.file.services["web"];
    assert_eq!(w.name.as_deref(), Some("dev-x"));
    assert_eq!(w.cpus.as_deref(), Some("8"));
    assert_eq!(w.labels["wt"], "/w");
}

#[test]
fn bare_environment_keys_come_from_the_environment() {
    let p = load_with(
            &["services:\n  web:\n    image: x\n    environment: [SET, UNSET, A=1]\n    exec: {env: [SET]}\n"],
            &[("SET", "yes")],
        )
        .unwrap();
    let w = &p.file.services["web"];
    assert_eq!(w.env.len(), 2);
    assert_eq!(w.env["SET"], "yes");
    assert_eq!(w.exec.env["SET"], "yes");
}

#[test]
fn unset_variable_is_an_error_naming_the_file() {
    let e = load_with(&["services:\n  web: {image: \"${IMG}\"}\n"], &[])
        .unwrap_err()
        .to_string();
    assert!(e.contains("f0.yaml") && e.contains("IMG"), "{e}");
}

#[test]
fn later_files_merge_over_earlier() {
    let p = load_with(
            &[
                "services:\n  web:\n    image: dev-base\n    cpus: 8\n    labels: [a=1]\n    environment: [X=1]\n    ports: [8080:80]\n    volumes: [./a:/a, ./b:/b]\n    command: [one]\n",
                "services:\n  web:\n    cpus: 4\n    labels: {b: '2'}\n    environment: [Y=2]\n    ports: [8080:80, 9090:90]\n    volumes: ['./c:/a/:ro']\n    command: two three\n",
            ],
            &[],
        )
        .unwrap();
    let w = &p.file.services["web"];
    assert_eq!(w.image, "dev-base");
    assert_eq!(w.cpus.as_deref(), Some("4"));
    assert_eq!(w.labels.len(), 2);
    assert_eq!(w.env.len(), 2);
    // Ports append (an identical entry once); volumes merge by target.
    assert_eq!(w.ports.len(), 2);
    assert_eq!(w.volumes.len(), 2);
    assert_eq!(w.volumes[0].source, "./c");
    assert!(w.volumes[0].read_only);
    assert_eq!(w.volumes[1].source, "./b");
    assert_eq!(w.command.as_deref().unwrap(), ["two", "three"]);
}

#[test]
fn extension_keys_anchors_and_version() {
    let p = load_with(
            &["version: '3.8'\nx-common: &common\n  image: dev-base\n  cpus: 2\nservices:\n  a:\n    <<: *common\n    x-note: hi\n  b:\n    <<: *common\n    cpus: 3\n"],
            &[],
        )
        .unwrap();
    assert_eq!(p.file.services["a"].cpus.as_deref(), Some("2"));
    assert_eq!(p.file.services["b"].cpus.as_deref(), Some("3"));
    assert_eq!(p.file.services["b"].image, "dev-base");
}

#[test]
fn unknown_fields_rejected() {
    let e = load_with(&["services:\n  web: {image: x, mem: 1}\n"], &[])
        .unwrap_err()
        .to_string();
    assert!(e.contains("mem"), "{e}");
    let e = load_with(&["service:\n  web: {image: x}\n"], &[])
        .unwrap_err()
        .to_string();
    assert!(e.contains("service"), "{e}");
}

#[test]
fn docker_only_keys_get_a_hint() {
    let hint = |doc: &str| load_with(&[doc], &[]).unwrap_err().to_string();
    let e = hint("services:\n  web: {image: x, build: .}\n");
    assert!(e.contains("`build`") && e.contains("image"), "{e}");
    let e = hint("services:\n  web: {image: x, env_file: a.env}\n");
    assert!(e.contains("environment"), "{e}");
    let e = hint("services:\n  web: {image: x, profiles: [dev]}\n");
    assert!(e.contains("incus_profiles"), "{e}");
    let e = hint("networks: {}\nservices: {}\n");
    assert!(e.contains("`networks`"), "{e}");
    let e = hint("sandboxes:\n  web: {image: x}\n");
    assert!(e.contains("services"), "{e}");
    let e = hint("services:\n  web: {image: x, exec: {user: dev}}\n");
    assert!(e.contains("user on the service"), "{e}");
}

#[test]
fn select_services() {
    let p = load_with(&["services:\n  a: {image: x}\n  b: {image: x}\n"], &[]).unwrap();
    assert_eq!(p.select(&[]).unwrap(), vec!["a", "b"]);
    assert_eq!(p.select(&["b".into()]).unwrap(), vec!["b"]);
    assert!(p.select(&["c".into()]).is_err());
}

#[test]
fn sanitizes_names() {
    assert_eq!(sanitize_name("My Project!"), "my-project");
    assert_eq!(sanitize_name("123"), "isb-123");
    assert_eq!(sanitize_name("--a--b--"), "a-b");
}
