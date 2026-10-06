//! Where the dashboard's data comes from: the `isb serve` daemon over its
//! unix socket, or, with no daemon, incus directly (sandboxes only, and only
//! what needs no controller).

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::model::{Change, Event, Overview, Sandbox};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::metrics::Sampler;

pub enum Source {
    Daemon { socket: PathBuf },
    Direct { sampler: Box<Sampler> },
}

const CALL: Duration = Duration::from_secs(30);

/// `name` and `org` arguments for a qualified stack name (`org/name`).
fn named(q: &str) -> Value {
    match q.split_once('/') {
        Some((org, name)) => json!({"org": org, "name": name}),
        None => json!({"name": q}),
    }
}

fn with(mut base: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().into_iter().flatten() {
        base[k] = v.clone();
    }
    base
}

impl Source {
    /// The daemon if it answers on `socket`, else incus directly.
    pub fn connect(socket: PathBuf) -> Source {
        match crate::serve_client::call_tool(
            &socket,
            "server_status",
            json!({}),
            Duration::from_secs(3),
        ) {
            Ok(_) => Source::Daemon { socket },
            Err(_) => Source::Direct {
                sampler: Box::new(Sampler::new()),
            },
        }
    }

    pub fn is_daemon(&self) -> bool {
        matches!(self, Source::Daemon { .. })
    }

    fn call(&self, tool: &str, args: Value, timeout: Duration) -> Result<Value> {
        match self {
            Source::Daemon { socket } => {
                crate::serve_client::call_tool(socket, tool, args, timeout)
            }
            Source::Direct { .. } => Err(Error::invalid(
                "that needs the isb serve daemon (start it with `isb serve`, or `isb serve install`)",
            )),
        }
    }

    pub fn overview(&mut self, client: &Client) -> Result<Overview> {
        match self {
            Source::Daemon { .. } => {
                let v = self.call("overview", json!({}), CALL)?;
                serde_json::from_value(v).map_err(|e| Error::Protocol(format!("overview: {e}")))
            }
            Source::Direct { sampler } => {
                let (host, insts) = sampler.sample(client)?;
                Ok(Overview {
                    isb: env!("CARGO_PKG_VERSION").into(),
                    host: host.into(),
                    stacks: Vec::new(),
                    sandboxes: insts
                        .into_iter()
                        .filter(|i| i.stack().is_none())
                        .map(Sandbox::from)
                        .collect(),
                    events_seq: 0,
                })
            }
        }
    }

    /// Events after `since`, waiting up to `wait` for one.
    pub fn events(&self, since: u64, wait: Duration) -> Result<(u64, Vec<Event>)> {
        if !self.is_daemon() {
            std::thread::sleep(wait);
            return Ok((since, Vec::new()));
        }
        let v = self.call(
            "events",
            json!({"since": since, "limit": 200, "wait": wait.as_secs()}),
            wait + CALL,
        )?;
        let seq = v["seq"].as_u64().unwrap_or(since);
        let events: Vec<Event> = serde_json::from_value(v["events"].clone()).unwrap_or_default();
        Ok((seq, events))
    }

    pub fn scale(&self, stack: &str, service: &str, replicas: u32) -> Result<()> {
        self.call(
            "stack_scale",
            with(
                named(stack),
                json!({"service": service, "replicas": replicas}),
            ),
            CALL,
        )
        .map(|_| ())
    }

    pub fn redeploy(&self, stack: &str, service: &str) -> Result<()> {
        self.call(
            "stack_redeploy",
            json!({"name": stack, "service": service}),
            CALL,
        )
        .map(|_| ())
    }

    pub fn rollback(&self, stack: &str) -> Result<()> {
        self.call("stack_rollback", named(stack), CALL).map(|_| ())
    }

    pub fn remove_stack(&self, stack: &str) -> Result<()> {
        self.call(
            "stack_remove",
            json!({"name": stack}),
            Duration::from_secs(400),
        )
        .map(|_| ())
    }

    /// Each replica's recent output, by instance.
    pub fn stack_logs(
        &self,
        stack: &str,
        service: &str,
        lines: usize,
    ) -> Result<Vec<(String, String)>> {
        let v = self.call(
            "stack_logs",
            with(named(stack), json!({"service": service, "lines": lines})),
            CALL,
        )?;
        Ok(v["logs"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A deploy's plan (`apply: false`) or the deploy itself.
    pub fn deploy(&self, mut args: Value, apply: bool) -> Result<Vec<Change>> {
        args["dry_run"] = json!(!apply);
        args["wait"] = json!(false);
        let v = self.call("stack_deploy", args, CALL)?;
        Ok(serde_json::from_value(v["changes"].clone()).unwrap_or_default())
    }
}
