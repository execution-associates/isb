//! The kubectl-shaped tools: who may call them, what they cap, and what
//! they leave in the audit log.

use super::super::tests::{token, user};
use super::*;
use std::collections::BTreeSet;

use crate::auth::{Principal, Role};
use crate::stack::controller::InstanceStatus;

fn ok(c: &Caller, tool: &str, org: &str) -> bool {
    authorize_class(
        c,
        tool,
        class_of(tool).expect("a kube tool"),
        json!({"org": org, "name": "web"}),
        None,
        false,
    )
    .is_ok()
}

fn all() -> impl Iterator<Item = &'static str> {
    READS.iter().chain(SECRET_READS).chain(WRITES).copied()
}

fn instance(name: &str, slot: u32, status: &str, health: &str, rotation: bool) -> InstanceStatus {
    InstanceStatus {
        name: name.into(),
        slot,
        rev: "r1".into(),
        status: status.into(),
        health: health.into(),
        ip: None,
        in_rotation: rotation,
        restarts: 0,
        last_probe: String::new(),
        cpu_pct: None,
        cpu_history: vec![],
        mem_bytes: None,
        disk_bytes: None,
    }
}

#[test]
fn every_tool_has_a_class_and_the_tables_do_not_overlap() {
    let mut seen = BTreeSet::new();
    for t in all() {
        assert!(seen.insert(t), "{t} is in two tables");
        assert!(class_of(t).is_some());
    }
    assert!(class_of("app_deploy").is_none());
    // Exec, restarts and writes change things; the file read can hold secrets.
    for t in WRITES {
        assert_eq!(class_of(t), Some(audit::Class::default()), "{t}");
    }
    assert!(class_of("instance_file_read").unwrap().secret_read);
    assert!(class_of("instance_list").unwrap().read_only);
}

#[test]
fn annotations_say_what_the_authorizer_judges() {
    let ann = Ann {
        ro: json!({"readOnlyHint": true, "openWorldHint": false}),
        destructive: json!({"destructiveHint": true}),
        write: json!({"destructiveHint": false, "openWorldHint": false}),
    };
    for t in all() {
        let tool =
            Tool::new(t, "", json!({}), |_, _| Ok(json!({}))).annotations(annotations(t, &ann));
        assert_eq!(audit::class(&tool), class_of(t).unwrap(), "{t}");
    }
}

#[test]
fn viewers_read_members_act_and_nobody_crosses_orgs() {
    let viewer = user(&[("acme", Role::Viewer)], false);
    let member = user(&[("acme", Role::Member)], false);
    for t in READS {
        assert!(ok(&viewer, t, "acme"), "{t}: a viewer reads");
        assert!(ok(&member, t, "acme"), "{t}");
        assert!(!ok(&member, t, "beta"), "{t}: another org");
    }
    for t in WRITES.iter().chain(SECRET_READS) {
        assert!(
            !ok(&viewer, t, "acme"),
            "{t}: a viewer does not run or change anything"
        );
        assert!(ok(&member, t, "acme"), "{t}: a member does");
        assert!(!ok(&member, t, "beta"), "{t}: another org");
        assert!(!ok(&member, t, "default"), "{t}: the default org");
    }
    // A viewer's admin token still reads only.
    let v = token(&[("acme", Role::Viewer)], &["admin"]);
    assert!(!ok(&v, "app_exec", "acme"));
    assert!(ok(&v, "app_logs", "acme"));
}

#[test]
fn a_workspace_token_acts_in_its_org_only() {
    let acme = OrgId::new("acme").unwrap();
    let ws = Caller::User {
        principal: Arc::new(Principal::workspace(&acme, "workspace", Role::Admin)),
    };
    for t in all() {
        assert!(ok(&ws, t, "acme"), "{t}");
        assert!(!ok(&ws, t, "beta"), "{t}: another org");
    }
    // An org-bound endpoint pins the org.
    let e = authorize_class(
        &ws,
        "app_exec",
        audit::Class::default(),
        json!({"org": "beta"}),
        Some(&acme),
        false,
    );
    assert!(e.is_err());
    let pinned = authorize_class(
        &ws,
        "app_exec",
        audit::Class::default(),
        json!({}),
        Some(&acme),
        false,
    )
    .unwrap();
    assert_eq!(pinned["org"], "acme");
    // A viewer workspace only reads.
    let viewer = Caller::User {
        principal: Arc::new(Principal::workspace(&acme, "workspace", Role::Viewer)),
    };
    assert!(ok(&viewer, "instance_list", "acme"));
    assert!(!ok(&viewer, "instance_exec", "acme"));
}

#[test]
fn token_scopes_narrow_exec_files_and_restarts() {
    let member = [("acme", Role::Member)];
    let read = token(&member, &["read"]);
    for t in READS {
        assert!(ok(&read, t, "acme"), "{t}");
    }
    for t in WRITES.iter().chain(SECRET_READS) {
        assert!(!ok(&read, t, "acme"), "{t}: read scope");
    }
    // `deploy` adds scaling and restarting, not running code or files.
    let deploy = token(&member, &["deploy"]);
    for t in ["app_scale", "app_restart", "instance_restart"] {
        assert!(ok(&deploy, t, "acme"), "{t}");
    }
    for t in [
        "app_exec",
        "instance_exec",
        "instance_file_write",
        "instance_file_read",
    ] {
        assert!(!ok(&deploy, t, "acme"), "{t}: deploy scope");
    }
    // A tool: scope can name exactly the tools.
    let only = token(&member, &["tool:app_exec", "tool:instance_list"]);
    assert!(ok(&only, "app_exec", "acme"));
    assert!(ok(&only, "instance_list", "acme"));
    assert!(!ok(&only, "instance_exec", "acme"));
    assert!(ok(&token(&member, &[]), "instance_file_write", "acme"));
}

#[test]
fn deny_tools_turns_them_off() {
    let p = ToolPolicy::from_lists("", "app_exec,instance_*");
    assert!(!p.allows("app_exec"));
    assert!(!p.allows("instance_file_write"));
    assert!(p.allows("app_logs"));
}

#[test]
fn output_keeps_the_end_and_says_so() {
    let mut t = Tail::new(10);
    for chunk in [
        b"0123456789".as_slice(),
        b"abcdefghij",
        b"KLMNOPQRST",
        b"uv",
    ] {
        t.push(chunk);
    }
    let (kept, truncated, total) = t.finish();
    assert_eq!(kept, b"MNOPQRSTuv".to_vec());
    assert!(truncated);
    assert_eq!(total, 32);
    let mut small = Tail::new(10);
    small.push(b"short");
    let (kept, truncated, total) = small.finish();
    assert_eq!(
        (kept.as_slice(), truncated, total),
        (b"short".as_slice(), false, 5)
    );
    // The cap is what the tools use.
    assert_eq!(EXEC_OUTPUT_CAP, 1024 * 1024);
}

#[test]
fn a_big_stream_is_held_to_the_cap_as_it_goes() {
    let mut t = Tail::new(1024);
    for _ in 0..1000 {
        t.push(&[b'x'; 4096]);
        assert!(t.buf.len() <= 3 * 1024 + 4096, "memory stays bounded");
    }
    let (kept, truncated, total) = t.finish();
    assert_eq!(kept.len(), 1024);
    assert!(truncated);
    assert_eq!(total, 4_096_000);
}

#[test]
fn timeouts_default_to_a_minute_and_stop_at_fifteen() {
    assert_eq!(exec_timeout(None).unwrap(), Duration::from_secs(60));
    assert_eq!(exec_timeout(Some("30s")).unwrap(), Duration::from_secs(30));
    assert_eq!(exec_timeout(Some("15m")).unwrap(), Duration::from_secs(900));
    for bad in ["16m", "2h", "0s", "soon"] {
        assert!(exec_timeout(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn an_exec_request_is_checked() {
    let run = |argv: Vec<&str>, stdin: Option<String>, env: &[(&str, &str)]| {
        Run::new(
            argv.into_iter().map(String::from).collect(),
            None,
            None,
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            stdin,
            None,
        )
    };
    assert!(run(vec!["ls", "-l"], None, &[]).is_ok());
    assert!(run(vec![], None, &[]).is_err());
    assert!(run(vec![""], None, &[]).is_err());
    assert!(run(vec!["a\0b"], None, &[]).is_err());
    assert!(run(vec!["cat"], Some("x".repeat(EXEC_STDIN_CAP)), &[]).is_ok());
    let e = run(vec!["cat"], Some("x".repeat(EXEC_STDIN_CAP + 1)), &[])
        .err()
        .unwrap();
    assert!(e.to_string().contains("stdin"), "{e}");
    assert!(run(vec!["env"], None, &[("A", "1")]).is_ok());
    assert!(run(vec!["env"], None, &[("A=B", "1")]).is_err());
    assert!(run(vec!["env"], None, &[("", "1")]).is_err());
}

#[test]
fn a_replica_is_chosen_by_slot_by_name_or_by_health() {
    let r = vec![
        instance("web-1", 1, "Running", "unhealthy", false),
        instance("web-2", 2, "Running", "healthy", true),
        instance("web-3", 3, "Running", "healthy", true),
        instance("web-4", 4, "Stopped", "none", false),
    ];
    // Default: running, in rotation, healthy, lowest slot.
    assert_eq!(pick_replica("web", &r, None, None).unwrap().slot, 2);
    assert_eq!(pick_replica("web", &r, Some(3), None).unwrap().slot, 3);
    assert_eq!(
        pick_replica("web", &r, None, Some("web-1")).unwrap().slot,
        1
    );
    // Asked for, but not running: say so rather than pick another.
    let e = pick_replica("web", &r, Some(4), None).unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
    let e = pick_replica("web", &r, Some(9), None).unwrap_err();
    assert!(
        e.is_not_found() && e.to_string().contains("replica 9 of web"),
        "{e}"
    );
    assert!(pick_replica("web", &r, Some(1), Some("web-1")).is_err());
    assert!(
        pick_replica("web", &r, None, Some("other"))
            .unwrap_err()
            .is_not_found()
    );
    // Nothing healthy: a running one out of rotation will do; none running: refused.
    let r = vec![
        instance("web-1", 1, "Running", "starting", false),
        instance("web-2", 2, "Stopped", "none", false),
    ];
    assert_eq!(pick_replica("web", &r, None, None).unwrap().slot, 1);
    let r = vec![instance("web-1", 1, "Stopped", "none", false)];
    let e = pick_replica("web", &r, None, None).unwrap_err();
    assert!(e.to_string().contains("no running replica"), "{e}");
    assert!(pick_replica("web", &[], None, None).is_err());
}

#[test]
fn file_paths_are_clean_and_isbs_own_files_are_off_limits() {
    assert_eq!(
        clean_path("/etc//app/./conf.d/a.conf").unwrap(),
        "/etc/app/conf.d/a.conf"
    );
    for bad in ["etc/passwd", "/etc/../root/x", "/", "//", "/a\0b"] {
        assert!(clean_path(bad).is_err(), "{bad}");
    }
    // The token and isb's directories are never written; the token is never read.
    for p in [
        "/run/isb/token",
        "/run/secrets/db",
        "/etc/isb/web.env",
        "/etc/isb",
    ] {
        assert!(refuse_path(p, true, &[]).is_some(), "{p}");
    }
    assert!(refuse_path("/run/isb/token", false, &[]).is_some());
    assert!(
        refuse_path("/run/secrets/db", false, &[]).is_none(),
        "readable (and audited)"
    );
    assert!(
        refuse_path("/run/isbx/file", true, &[]).is_none(),
        "a prefix is not a parent"
    );
    // Files isb delivers from org secrets, wherever they are.
    let managed = vec!["/app/config/tls.pem".to_string()];
    assert!(refuse_path("/app/config/tls.pem", true, &managed).is_some());
    assert!(refuse_path("/app/config/other.pem", true, &managed).is_none());
    // The kernel's trees.
    for p in ["/proc/1/environ", "/sys/kernel/x", "/dev/null"] {
        assert!(refuse_path(p, false, &[]).is_some(), "{p}");
        assert!(refuse_path(p, true, &[]).is_some(), "{p}");
    }
    assert!(refuse_path("/home/dev/notes.txt", true, &managed).is_none());
}

#[test]
fn file_sizes_are_capped() {
    assert_eq!(FILE_READ_CAP, 4 * 1024 * 1024);
    assert_eq!(FILE_WRITE_CAP, 2 * 1024 * 1024);
    assert_eq!(parse_mode(&json!("0644")).unwrap(), 0o644);
    assert_eq!(parse_mode(&json!("755")).unwrap(), 0o755);
    assert_eq!(parse_mode(&json!(600)).unwrap(), 0o600);
    assert!(parse_mode(&json!("0999")).is_err());
    assert!(parse_mode(&json!("17777")).is_err());
    assert!(parse_mode(&json!(true)).is_err());
}

#[test]
fn since_keeps_the_lines_from_then_on() {
    let log = "2026-10-04T10:00:00+0000 h app[1]: old\n\
               2026-10-04T10:59:00+0000 h app[1]: newer\n\
               a continuation line\n\
               2026-10-04T11:00:00+0000 h app[1]: newest";
    let cut = crate::history::rfc3339_ms("2026-10-04T10:30:00Z").unwrap();
    let (kept, seen) = since_lines(log, cut);
    assert!(seen);
    assert!(!kept.contains("old"));
    assert!(kept.contains("newer") && kept.contains("continuation") && kept.contains("newest"));
    // No timestamps (an OCI console): everything stays, and the caller is told.
    let (kept, seen) = since_lines("a\nb", cut);
    assert_eq!(kept, "a\nb");
    assert!(!seen);
}

#[test]
fn an_instance_is_an_app_a_database_a_tunnel_or_the_rest() {
    let l = |k: &str| BTreeMap::from([(k.to_string(), String::new())]);
    assert_eq!(kind(&l("isb.stack"), false, Some(false)), "app");
    assert_eq!(kind(&l("isb.stack"), false, Some(true)), "database");
    assert_eq!(kind(&l("isb.stack"), false, None), "stack");
    assert_eq!(kind(&l("isb.stack"), true, None), "tunnel");
    assert_eq!(kind(&l("isb.workspace"), false, None), "workspace");
    assert_eq!(kind(&l("isb.build"), false, None), "build");
    assert_eq!(kind(&BTreeMap::new(), false, None), "sandbox");
}

#[test]
fn the_audit_row_has_the_argv_and_the_path_but_not_the_secrets() {
    use crate::audit::Origin;
    use crate::server::mcp::Audited;
    let ann = Ann {
        ro: json!({"readOnlyHint": true}),
        destructive: json!({}),
        write: json!({"destructiveHint": false}),
    };
    let c = user(&[("acme", Role::Member)], false);
    let origin = Origin::default();
    let entry_for = |name: &str, args: Value, outcome: std::result::Result<(), &Error>| {
        let tool = Tool::new(name, "", json!({}), |_, _| Ok(json!({})))
            .annotations(annotations(name, &ann));
        audit::entry(
            &Audited {
                caller: &c,
                action: name,
                tool: Some(&tool),
                args: &args,
                outcome,
                origin: &origin,
            },
            false,
        )
        .expect("recorded")
    };
    let e = entry_for(
        "app_exec",
        json!({"org": "acme", "name": "web", "replica": 2, "argv": ["psql", "-c", "select 1"],
               "env": {"PGPASSWORD": "hunter2"}, "stdin": "top secret input"}),
        Ok(()),
    );
    assert_eq!(e.action, "app_exec");
    assert_eq!(e.target.as_deref(), Some("web"));
    assert_eq!(e.details["argv"], json!(["psql", "-c", "select 1"]));
    assert_eq!(e.details["replica"], 2);
    assert_eq!(e.details["env_keys"], json!(["PGPASSWORD"]));
    assert_eq!(e.details["stdin_bytes"], 16);
    let text = e.details.to_string();
    for secret in ["hunter2", "top secret input"] {
        assert!(!text.contains(secret), "{secret} leaked into {text}");
    }
    let e = entry_for(
        "instance_file_write",
        json!({"org": "acme", "name": "web-1", "path": "/srv/app.conf", "content": "token=abc", "mode": "0600"}),
        Ok(()),
    );
    assert_eq!(e.details["path"], "/srv/app.conf");
    assert_eq!(e.details["bytes"], 9);
    assert!(!e.details.to_string().contains("token=abc"));
    let e = entry_for(
        "instance_file_write",
        json!({"org": "acme", "name": "web-1", "path": "/f", "content": "aGk=", "encoding": "base64"}),
        Ok(()),
    );
    assert_eq!(e.details["bytes"], 2);
    // A read of a file is recorded, whatever it returns.
    let e = entry_for(
        "instance_file_read",
        json!({"org": "acme", "name": "web-1", "path": "/run/secrets/db"}),
        Ok(()),
    );
    assert_eq!(e.details["path"], "/run/secrets/db");
    // A refusal is recorded too.
    let denied = Error::Forbidden("a viewer only reads".into());
    let e = entry_for(
        "instance_exec",
        json!({"org": "acme", "name": "web-1", "argv": ["id"]}),
        Err(&denied),
    );
    assert_eq!(e.outcome, "forbidden");
    assert_eq!(e.details["argv"], json!(["id"]));
}

#[test]
fn a_control_plane_sends_them_to_the_server_the_org_lives_on() {
    use super::super::servers::{Way, decide};
    let placed = |o: &OrgId| (o.as_str() == "far").then(|| "box".to_string());
    for t in all() {
        assert_eq!(
            decide(t, &json!({"org": "far"}), &placed),
            Way::Forward("box".into(), OrgId::new("far").unwrap()),
            "{t}"
        );
        assert_eq!(
            decide(t, &json!({"org": "near"}), &placed),
            Way::Here,
            "{t}"
        );
    }
}
