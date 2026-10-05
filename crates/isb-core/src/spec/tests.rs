use super::*;

fn parse(y: &str) -> Result<ComposeFile, String> {
    serde_yaml_ng::from_str(y).map_err(|e| e.to_string())
}

#[test]
fn rejects_unknown_fields() {
    let e = parse("services:\n  web:\n    image: x\n    cpu: 8\n").unwrap_err();
    assert!(e.contains("unknown field `cpu`"), "{e}");
    let e = parse(
        "services:\n  web:\n    image: x\n    volumes:\n      - {source: /b, target: /a, bnd: 1}\n",
    )
    .unwrap_err();
    assert!(e.contains("bnd"), "{e}");
}

#[test]
fn accepts_strings_for_scalars() {
    let f = parse(
        "services:\n  web:\n    image: x\n    cpus: \"8\"\n    privileged: \"false\"\n    ports:\n      - {published: '5173-5223', target: '5173', host_ip: 1.2.3.4}\n",
    )
    .unwrap();
    let w = &f.services["web"];
    assert_eq!(w.cpus.as_deref(), Some("8"));
    assert_eq!(w.privileged, Some(false));
    assert_eq!(w.ports[0].search, Some(50));
    assert_eq!(w.ports[0].listen, "tcp:1.2.3.4:5173");
    let f = parse("services:\n  web:\n    image: x\n    cpus: 4\n").unwrap();
    assert_eq!(f.services["web"].cpus.as_deref(), Some("4"));
}

#[test]
fn scalar_values_in_string_maps() {
    let f = parse(
        "services:\n  web:\n    image: x\n    environment: {DEBUG: 1, ON: true}\n    raw_config: {security.nesting: true}\n    raw_devices: {gpu: {type: gpu, id: 0}}\n    user: 1000\n",
    )
    .unwrap();
    let w = &f.services["web"];
    assert_eq!(w.env["DEBUG"], "1");
    assert_eq!(w.env["ON"], "true");
    assert_eq!(w.raw_config["security.nesting"], "true");
    assert_eq!(w.raw_devices["gpu"]["id"], "0");
    assert_eq!(w.user.as_deref(), Some("1000"));
}

#[test]
fn list_forms_of_environment_and_labels() {
    let f = parse(
        "services:\n  web:\n    image: x\n    environment: [A=1, B=x=y]\n    labels: [k=v, bare]\n    exec: {env: [C=3]}\n",
    )
    .unwrap();
    let w = &f.services["web"];
    assert_eq!(w.env["A"], "1");
    assert_eq!(w.env["B"], "x=y");
    assert_eq!(w.labels["k"], "v");
    assert_eq!(w.labels["bare"], "");
    assert_eq!(w.exec.env["C"], "3");
    let e = parse("services:\n  web: {image: x, environment: [NOVALUE]}\n").unwrap_err();
    assert!(e.contains("NOVALUE"), "{e}");
}

#[test]
fn secret_variables_as_a_file() {
    let f = parse(
        "services:\n  web:\n    image: x\n    environment:\n      A: {secret: a}\n      B: {secret: b, as: env}\n      C: {secret: c, as: file, on_change: restart}\n",
    )
    .unwrap();
    let w = &f.services["web"];
    assert_eq!(w.env.secrets["A"], "a");
    assert_eq!(w.env.secrets["B"], "b");
    assert_eq!(w.env.files["C"], "c");
    assert!(!w.env.secrets.contains_key("C"));
    assert_eq!(w.secret_on_change("c"), Some(OnChange::Restart));
    assert!(w.secret_keys().contains("c"));
    assert!(w.has_secret_files());
    assert_eq!(
        w.env.file_vars().collect::<Vec<_>>(),
        [("C_FILE".to_string(), "/run/secrets/c".to_string())]
    );
    // `as: env` is the default and serializes as it always did; `as: file`
    // round-trips.
    let y = serde_yaml_ng::to_string(&w.env).unwrap();
    assert!(!y.contains("as: env"), "{y}");
    let back: Environment = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(back, w.env);
    assert!(
        parse("services:\n  web: {image: x, environment: {A: {secret: a, as: disk}}}\n").is_err()
    );
}

#[test]
fn volume_forms() {
    let f = parse(
        "services:\n  web:\n    image: x\n    volumes:\n      - ./src:/home/dev/src:ro\n      - cache:/home/dev/.cache:owner=dev\n      - {type: bind, source: ~/ref, target: /srv/ref, read_only: true, options: {shift: true}}\n      - {source: data, target: /data, device: d}\n",
    )
    .unwrap();
    let v = &f.services["web"].volumes;
    assert_eq!(v.len(), 4);
    assert_eq!(
        (
            v[0].mount_type,
            v[0].source.as_str(),
            v[0].target.as_str(),
            v[0].read_only
        ),
        (MountType::Bind, "./src", "/home/dev/src", true)
    );
    assert_eq!(v[1].mount_type, MountType::Volume);
    assert_eq!(v[1].owner.as_deref(), Some("dev"));
    assert_eq!(v[2].options["shift"], "true");
    // The long form infers the type from the source, like the short form.
    assert_eq!(v[3].mount_type, MountType::Volume);
    assert!(parse("services:\n  web: {image: x, volumes: [/anon]}\n").is_err());
    assert!(parse("services:\n  web: {image: x, volumes: [{source: a}]}\n").is_err());
}

#[test]
fn port_forms() {
    let f = parse(
        "services:\n  web:\n    image: x\n    ports:\n      - 8080:80\n      - \"${IP}:5173:5173/udp\"\n      - {target: 80, published: 8081}\n      - {name: backend, bind: guest, listen: 8190, connect: \"8080\"}\n",
    )
    .unwrap();
    let p = &f.services["web"].ports;
    assert_eq!(
        (p[0].listen.as_str(), p[0].connect.as_str()),
        ("tcp:127.0.0.1:8080", "tcp:80")
    );
    assert_eq!(p[1].listen, "udp:${IP}:5173");
    assert_eq!(p[2].listen, "tcp:127.0.0.1:8081");
    assert_eq!(p[3].bind, PortBind::Guest);
    assert_eq!(
        (p[3].listen.as_str(), p[3].connect.as_str()),
        ("8190", "8080")
    );
    let e = parse("services:\n  web: {image: x, ports: [5173]}\n").unwrap_err();
    assert!(e.contains("host port"), "{e}");
    let e = parse("services:\n  web: {image: x, ports: [{listen: 1, connect: 2, search: 5}]}\n")
        .unwrap_err();
    assert!(e.contains("published"), "{e}");
}

#[test]
fn ports_serialize_back_to_what_parses() {
    let f = parse(
        "services:\n  web:\n    image: x\n    ports:\n      - 100.1.2.3:5173-5223:5173\n      - 53:53/udp\n      - 8000-8002:9000-9002\n      - {bind: guest, listen: 8190, connect: 8080}\n      - {listen: 'tcp:0.0.0.0:80', connect: 'tcp:10.0.0.2:80'}\n",
    )
    .unwrap();
    let y = serde_yaml_ng::to_string(&f).unwrap();
    assert!(y.contains("published: 5173-5223"), "{y}");
    assert!(y.contains("protocol: udp"), "{y}");
    let back: ComposeFile = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(back, f);
}

#[test]
fn volumes_serialize_back_to_what_parses() {
    let f = parse(
        "services:\n  web:\n    image: x\n    volumes: [./a:/a:ro, 'c:/c:owner=dev,device=d']\n",
    )
    .unwrap();
    let y = serde_yaml_ng::to_string(&f).unwrap();
    let back: ComposeFile = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(back, f);
}

#[test]
fn command_as_a_string() {
    let f = parse("services:\n  a: {image: x, command: \"sh -c 'bun install && bun run dev'\"}\n")
        .unwrap();
    assert_eq!(
        f.services["a"].command.as_deref().unwrap(),
        ["sh", "-c", "bun install && bun run dev"]
    );
}

#[test]
fn idmap_forms() {
    let f = parse(
        "services:\n  a: {image: x, idmap: auto}\n  b: {image: x, idmap: {raw: 'both 1 1'}}\n  c: {image: x, idmap: {mode: always, host_uid: 1001}}\n",
    )
    .unwrap();
    assert_eq!(
        f.services["a"].idmap,
        Some(IdmapSpec::Mode(IdmapMode::Auto))
    );
    assert!(matches!(f.services["b"].idmap, Some(IdmapSpec::Raw(_))));
    match &f.services["c"].idmap {
        Some(IdmapSpec::Map(m)) => {
            assert_eq!(m.mode, IdmapMode::Always);
            assert_eq!(m.host_uid, 1001);
            assert_eq!(m.guest_uid, 1000);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ready_forms() {
    let f = parse(
        "services:\n  a:\n    image: x\n    ready: [running, default_route, {user_exists: dev}, {path_writable: /x}, {command: [true]}]\n",
    )
    .unwrap();
    let r = f.services["a"].ready.as_ref().unwrap();
    assert_eq!(r.len(), 5);
    assert_eq!(r[4], ReadyCheck::Command(vec!["true".into()]));
    assert!(
        parse("services:\n  a: {image: x, ready: [bogus]}\n")
            .unwrap_err()
            .contains("bogus")
    );
}

#[test]
fn command_items_may_be_unquoted_scalars() {
    let f = parse(
        "services:\n  a:\n    image: x\n    command: [python3, -m, http.server, 8000, true, 1.5]\n",
    )
    .unwrap();
    assert_eq!(
        f.services["a"].command.as_deref().unwrap(),
        ["python3", "-m", "http.server", "8000", "true", "1.5"]
    );
}

#[test]
fn schema_generates() {
    let s = compose_schema();
    assert!(s.to_string().contains("services"));
}

#[test]
fn schema_lists_every_instance_type_serde_accepts() {
    let s = compose_schema();
    let consts: Vec<String> = s["$defs"]["InstanceType"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["const"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(consts, ["container", "virtual-machine", "vm"]);
    // Every value the schema lists must parse, and nothing else.
    for c in &consts {
        serde_json::from_value::<InstanceType>(serde_json::json!(c)).unwrap();
    }
    assert!(serde_json::from_value::<InstanceType>(serde_json::json!("lxc")).is_err());
    assert!(
        s["$defs"]["SandboxSpec"]["properties"]["type"]
            .to_string()
            .contains("InstanceType")
    );
}
