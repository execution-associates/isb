//! `isb volume snapshot ...`, `isb volume restore ...` and friends: a named
//! volume's snapshots and staged restores through the local daemon
//! (docs/volumes.md). Volume backups are `isb backup create --volume`.

use std::time::Duration;

use clap::Subcommand;
use serde_json::{Value, json};

use isb::Result;

use super::{SHORT, call, print_json, table};

fn with_org(org: &Option<String>, mut args: Value) -> Value {
    if let Some(o) = org {
        args["org"] = json!(o);
    }
    args
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

#[derive(Subcommand)]
pub enum SnapshotCmd {
    /// Snapshot a volume now (the pre-snapshot hook runs first) and follow it.
    Create {
        /// The volume.
        name: String,
        /// The snapshot's name (default manual-<stamp>).
        #[arg(long = "as")]
        snapshot: Option<String>,
        /// Return once it has started.
        #[arg(short, long)]
        detach: bool,
    },
    /// List a volume's snapshots, newest first.
    #[command(alias = "list")]
    Ls {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a snapshot.
    #[command(alias = "remove")]
    Rm { name: String, snapshot: String },
    /// Set the snapshot schedule, retention and hook.
    Schedule {
        name: String,
        /// Cron (five fields) or @hourly, @daily, ...
        #[arg(long, conflicts_with = "off")]
        schedule: Option<String>,
        /// Remove the schedule.
        #[arg(long)]
        off: bool,
        #[arg(long)]
        timezone: Option<String>,
        /// Scheduled (auto-*) snapshots kept (default 7).
        #[arg(long)]
        keep: Option<u32>,
        /// How long /etc/isb/pre-snapshot may run (default 5m).
        #[arg(long)]
        hook_timeout: Option<String>,
        /// A failing hook stops the snapshot (default: reported only).
        #[arg(long, conflicts_with = "hook_optional")]
        hook_required: bool,
        #[arg(long)]
        hook_optional: bool,
    },
    /// Snapshot runs, newest first.
    Runs { name: String },
    /// A snapshot run's log (default: the latest).
    Logs { name: String, run: Option<u64> },
}

/// What `isb volume` does through the daemon.
pub enum Cmd {
    Snapshot(SnapshotCmd),
    Show(String),
    Restore(RestoreArgs),
    Restores(Option<String>),
    Discard(String, String),
}

#[derive(clap::Args)]
pub struct RestoreArgs {
    /// The volume.
    pub name: String,
    /// From this snapshot.
    #[arg(long, conflicts_with_all = ["backup", "destination"])]
    pub snapshot: Option<String>,
    /// From this volume backup (its newest file, or --key).
    #[arg(long, conflicts_with = "destination")]
    pub backup: Option<String>,
    /// From a destination and --key (a backup deleted since).
    #[arg(long, requires = "key")]
    pub destination: Option<String>,
    #[arg(long)]
    pub key: Option<String>,
    /// Mount it in this instance (default: the one using the volume).
    #[arg(long)]
    pub instance: Option<String>,
    #[arg(short, long)]
    pub detach: bool,
}

pub fn run(org: &Option<String>, cmd: Cmd) -> Result<u8> {
    let c = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        Cmd::Snapshot(s) => snapshot(org, s),
        Cmd::Show(name) => {
            print_json(&c("volume_get", json!({"name": name}))?);
            Ok(0)
        }
        Cmd::Restore(a) => restore(org, a),
        Cmd::Restores(name) => {
            let r = c("volume_restore_list", json!({"name": name}))?;
            let mut rows = vec![vec![
                "OF".into(),
                "STAMP".into(),
                "VOLUME".into(),
                "FROM".into(),
                "MOUNTED".into(),
            ]];
            for x in r["restores"].as_array().into_iter().flatten() {
                let at = match (x["attached"].as_bool(), x["instance"].as_str()) {
                    (Some(true), Some(i)) => format!("{i}:{}", s(&x["path"])),
                    _ => "detached".into(),
                };
                rows.push(vec![
                    s(&x["of"]),
                    s(&x["stamp"]),
                    s(&x["volume"]),
                    s(&x["from"]),
                    at,
                ]);
            }
            table(rows);
            Ok(0)
        }
        Cmd::Discard(name, stamp) => {
            c(
                "volume_restore_discard",
                json!({"name": name, "stamp": stamp}),
            )?;
            eprintln!("discarded the restore {stamp} of {name}");
            Ok(0)
        }
    }
}

fn snapshot(org: &Option<String>, cmd: SnapshotCmd) -> Result<u8> {
    let c = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        SnapshotCmd::Create {
            name,
            snapshot,
            detach,
        } => {
            let r = c(
                "volume_snapshot_create",
                json!({"name": name, "snapshot": snapshot}),
            )?;
            let id = r["run"]["id"].as_u64().unwrap_or(0);
            eprintln!("snapshot of {name}: run {id} started");
            if detach {
                return Ok(0);
            }
            follow(org, "volume_snapshot_run_log", json!({"name": name}), id)
        }
        SnapshotCmd::Ls { name, json } => {
            let r = c("volume_snapshot_list", json!({"name": name}))?;
            if json {
                print_json(&r["snapshots"]);
                return Ok(0);
            }
            let mut rows = vec![vec!["NAME".into(), "KIND".into(), "CREATED".into()]];
            for x in r["snapshots"].as_array().into_iter().flatten() {
                rows.push(vec![s(&x["name"]), s(&x["kind"]), s(&x["created_at"])]);
            }
            table(rows);
            Ok(0)
        }
        SnapshotCmd::Rm { name, snapshot } => {
            c(
                "volume_snapshot_delete",
                json!({"name": name, "snapshot": snapshot}),
            )?;
            Ok(0)
        }
        cmd @ SnapshotCmd::Schedule { .. } => schedule(org, cmd),
        SnapshotCmd::Runs { name } => {
            let r = c("volume_snapshot_runs", json!({"name": name}))?;
            let mut rows = vec![vec![
                "RUN".into(),
                "STATUS".into(),
                "TRIGGER".into(),
                "SNAPSHOT".into(),
                "ERROR".into(),
            ]];
            for x in r["runs"].as_array().into_iter().flatten() {
                rows.push(vec![
                    x["id"].to_string(),
                    s(&x["status"]),
                    s(&x["trigger"]),
                    s(&x["detail"]["snapshot"]),
                    s(&x["error"]),
                ]);
            }
            table(rows);
            Ok(0)
        }
        SnapshotCmd::Logs { name, run } => {
            let id = match run {
                Some(r) => r,
                None => {
                    c("volume_snapshot_runs", json!({"name": name, "limit": 1}))?["runs"][0]["id"]
                        .as_u64()
                        .ok_or_else(|| {
                            isb::Error::Invalid(format!("{name} has no snapshot runs"))
                        })?
                }
            };
            let r = c("volume_snapshot_run_log", json!({"name": name, "run": id}))?;
            print!("{}", s(&r["text"]));
            Ok(0)
        }
    }
}

fn schedule(org: &Option<String>, cmd: SnapshotCmd) -> Result<u8> {
    let c = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    let SnapshotCmd::Schedule {
        name,
        schedule,
        off,
        timezone,
        keep,
        hook_timeout,
        hook_required,
        hook_optional,
    } = cmd
    else {
        return Ok(0);
    };
    let mut a = json!({"name": name});
    if off {
        a["schedule"] = Value::Null;
    } else if let Some(x) = schedule {
        a["schedule"] = json!(x);
    }
    for (k, v) in [("timezone", timezone), ("hook_timeout", hook_timeout)] {
        if let Some(v) = v {
            a[k] = json!(v);
        }
    }
    if let Some(k) = keep {
        a["keep"] = json!(k);
    }
    if hook_required || hook_optional {
        a["hook_required"] = json!(hook_required);
    }
    let r = c("volume_snapshot_schedule", a)?;
    let next = s(&r["next_run"]);
    eprintln!(
        "{name}: next snapshot {}",
        if next.is_empty() {
            "never".into()
        } else {
            next
        }
    );
    Ok(0)
}

fn restore(org: &Option<String>, a: RestoreArgs) -> Result<u8> {
    let args = json!({
        "name": a.name, "snapshot": a.snapshot, "backup": a.backup,
        "destination": a.destination, "key": a.key, "instance": a.instance,
    });
    let args: serde_json::Map<String, Value> = args
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let r = call("volume_restore", with_org(org, Value::Object(args)), SHORT)?;
    let id = r["run"]["id"].as_u64().unwrap_or(0);
    eprintln!(
        "restoring {} into {} (restore run {id}); it will be at {}",
        a.name,
        s(&r["staged"]["volume"]),
        s(&r["staged"]["path"])
    );
    if a.detach {
        return Ok(0);
    }
    follow(org, "backup_run_log", json!({"restore": true}), id)
}

/// Follow a run's log until it finishes.
fn follow(org: &Option<String>, tool: &str, mut args: Value, id: u64) -> Result<u8> {
    let mut offset = 0u64;
    args["run"] = json!(id);
    loop {
        args["offset"] = json!(offset);
        let r = call(tool, with_org(org, args.clone()), SHORT)?;
        print!("{}", s(&r["text"]));
        offset = r["offset"].as_u64().unwrap_or(offset);
        if r["finished"].as_bool() == Some(true) {
            let st = s(&r["run"]["status"]);
            match r["run"]["error"].as_str() {
                Some(e) => eprintln!("run {id}: {st} ({e})"),
                None => eprintln!("run {id}: {st}"),
            }
            return Ok(if st == "succeeded" { 0 } else { 1 });
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
