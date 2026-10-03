//! The daemon client (isb-core's `serve_client`, here as `server::client`)
//! end to end against a real server on a unix socket.

use super::client::*;
use crate::error::Error;
use crate::server::{Listener, Registry, Shutdown, Tool, ToolPolicy, serve_until};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn end_to_end_over_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("run/isb.sock");
    let mut reg = Registry::new();
    reg.register(Tool::new(
        "add",
        "Add two numbers",
        json!({"type": "object"}),
        |a, c| {
            let x = a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0);
            Ok(json!({"sum": x, "trusted": c.is_trusted()}))
        },
    ))
    .unwrap();
    reg.register(Tool::new("len", "", json!({}), |a, _| {
        Ok(json!(a["s"].as_str().unwrap_or("").len()))
    }))
    .unwrap();
    reg.register(Tool::new("gone", "", json!({}), |_, _| {
        Err(Error::NotFound("stack web".into()))
    }))
    .unwrap();
    reg.register(Tool::new("hidden", "", json!({}), |_, _| Ok(json!({}))))
        .unwrap();

    let shutdown = Shutdown::new();
    let stop = shutdown.clone();
    let s = sock.clone();
    let server = std::thread::spawn(move || {
        serve_until(
            vec![Listener::unix(s).policy(ToolPolicy::from_lists("", "hid*"))],
            reg,
            Arc::new(|| (true, json!({"ok": true}))),
            stop,
        )
    });
    let t0 = Instant::now();
    while !sock.exists() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let to = Duration::from_secs(10);

    let v = call_tool(&sock, "add", json!({"a": 2, "b": 3}), to).unwrap();
    assert_eq!(v, json!({"sum": 5, "trusted": true}));
    // A non-object result is unwrapped again.
    assert_eq!(
        call_tool(&sock, "len", json!({"s": "abcd"}), to).unwrap(),
        json!(4)
    );

    let e = call_tool(&sock, "gone", json!({}), to).unwrap_err();
    assert!(e.is_not_found(), "{e:?}");
    assert_eq!(e.to_string(), "stack web not found");

    let e = call_tool(&sock, "hidden", json!({}), to).unwrap_err();
    assert!(
        matches!(e, Error::Invalid(ref m) if m.contains("unknown tool")),
        "{e:?}"
    );

    let names: Vec<String> = list_tools(&sock, to)
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["add", "len", "gone"]);

    // Concurrent callers each get their own connection and answer.
    let workers: Vec<_> = (0..16)
        .map(|i| {
            let s = sock.clone();
            std::thread::spawn(move || {
                call_tool(&s, "add", json!({"a": i, "b": 1}), to).unwrap()["sum"]
                    .as_i64()
                    .unwrap()
            })
        })
        .collect();
    for (i, w) in workers.into_iter().enumerate() {
        assert_eq!(w.join().unwrap(), i as i64 + 1);
    }

    shutdown.trigger();
    server.join().unwrap().unwrap();
    assert!(!sock.exists(), "socket removed on shutdown");
    let e = call_tool(&sock, "add", json!({}), to).unwrap_err();
    assert!(e.to_string().contains("cannot connect to isb serve"), "{e}");
}

#[test]
fn healthz_over_tcp() {
    let shutdown = Shutdown::new();
    // Find a free port, then serve on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let (a, stop) = (addr.clone(), shutdown.clone());
    let server = std::thread::spawn(move || {
        serve_until(
            vec![Listener::tcp(a).allow_unauthenticated(true)],
            Registry::new(),
            Arc::new(|| (false, json!({"ok": false, "why": "starting"}))),
            stop,
        )
    });
    let t0 = Instant::now();
    let (status, body) = loop {
        match healthz(&addr, Duration::from_secs(2)) {
            Ok(r) => break r,
            Err(_) if t0.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(status, 503);
    assert_eq!(body["why"], "starting");
    shutdown.trigger();
    server.join().unwrap().unwrap();
}
