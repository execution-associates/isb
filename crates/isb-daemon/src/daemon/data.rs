//! The data tools: databases (`database_*`, over apps with a database
//! source), backups and restores (`backup_*`), and scheduled jobs
//! (`job_*`). Every one acts in the org it names, so the authorizer's org
//! check covers them; destinations on this host need a trusted caller.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{args, caller_name, obj};
use crate::app::{AppSpec, Apps, Source};
use crate::backup::{BackupSpec, Backups, Destination, RestoreRequest};
use crate::error::{Error, Result};
use crate::jobs::{JobSpec, Jobs, RunStore};
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};

/// What the data tools work on.
#[derive(Clone)]
pub struct Ctx {
    pub apps: Apps,
    pub jobs: Jobs,
    pub backups: Backups,
}

fn org_of(a: &Value) -> Result<OrgId> {
    super::arg_org(a)
}

/// Remove the keys a tool handles itself before deserializing the rest.
fn take(a: &mut Value, keys: &[&str]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if let Some(o) = a.as_object_mut() {
        for k in keys {
            if let Some(v) = o.remove(*k) {
                out.insert((*k).to_string(), v);
            }
        }
    }
    out
}

/// The caller may point a destination at this host: a superadmin (the
/// local CLI included) or a platform admin.
fn trusted(c: &Caller) -> bool {
    c.is_trusted() || c.principal().is_some_and(|p| p.platform_admin)
}

pub(super) fn unix_rfc3339(t: Option<i64>) -> Value {
    match t {
        Some(t) => json!(crate::cron::rfc3339(t)),
        None => Value::Null,
    }
}

/// Wait (bounded) until run `id` in `store` finishes.
pub(super) fn wait_run(store: &RunStore, id: u64, timeout: Duration) -> Result<crate::jobs::Run> {
    let started = Instant::now();
    loop {
        let r = store.get(id)?;
        if r.status.finished() || started.elapsed() >= timeout {
            return Ok(r);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(super) fn timeout_arg(t: &Option<String>, default: Duration) -> Result<Duration> {
    match t {
        Some(t) => crate::flex::parse_duration(t)
            .map_err(Error::invalid)
            .map(|d| d.min(Duration::from_secs(3600))),
        None => Ok(default),
    }
}

/// Volume backups, like the rest of a volume's care, are for org admins.
fn volume_backup_admin(x: &Ctx, org: &OrgId, name: &str, c: &Caller) -> Result<()> {
    if x.backups
        .get(org, name)
        .is_ok_and(|b| b.spec.volume.is_some())
    {
        super::volumes::require_admin(c, org, "changing or running a volume backup")?;
    }
    Ok(())
}

/// A database app with its connection details (password as a secret
/// reference; with `password`, the value too).
pub fn database_json(org: &OrgId, a: &crate::app::App, password: Option<&str>) -> Value {
    let mut v = super::apps::app_json(org, a);
    if let Source::Database(db) = &a.spec.source {
        v["connection"] = crate::app::database::connection(&a.spec, db, org, password);
    }
    v
}

/// The properties backup_create and backup_update share.
fn backup_props() -> Value {
    json!({
        "name": {"type": "string"},
        "database": {"type": "string", "description": "The database app (or give `volume`)."},
        "volume": {"type": "string", "description": "Or a named volume in the org: its snapshot is exported (incus' tar) and streamed to the bucket; restore with volume_restore."},
        "destination": {"type": "string"},
        "schedule": {"type": "string", "description": "Cron: five fields (minute hour day-of-month month day-of-week) or @hourly, @daily, @weekly, @monthly, @yearly."},
        "timezone": {"type": "string", "description": "UTC (default) or a fixed offset such as +02:00."},
        "keep": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "Backups kept in the bucket (default 7)."},
        "compression": {"type": "string", "enum": ["gzip", "zstd", "none"]},
        "enabled": {"type": "boolean"},
        "missed_grace": {"type": "string", "description": "How late a slot missed while the daemon was down still runs (default 1h)."}
    })
}

/// The properties job_create and job_update share.
fn job_props() -> Value {
    json!({
        "name": {"type": "string"},
        "schedule": {"type": "string", "description": "Cron: five fields or @hourly, @daily, @weekly, @monthly, @yearly."},
        "timezone": {"type": "string", "description": "UTC (default) or a fixed offset such as +02:00."},
        "target": {"type": "object", "description": "{app: NAME}, or {stack: NAME, service: NAME}."},
        "mode": {"type": "string", "enum": ["exec", "run"], "description": "exec (default): in a running replica. run: in a fresh one-off instance from the service's image, env and secrets, deleted after."},
        "command": {"description": "argv (a list), or a line split like a shell would (no shell runs unless you run one)."},
        "timeout": {"type": "string", "description": "Kill after this long (default 10m, at most 24h)."},
        "concurrency": {"type": "string", "enum": ["skip", "allow"], "description": "skip (default): a run due while one is going is skipped."},
        "keep": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "Runs kept (default 20)."},
        "enabled": {"type": "boolean"},
        "user": {"type": "string"},
        "cwd": {"type": "string"},
        "env": {"type": "object", "additionalProperties": {"type": "string"}},
        "missed_grace": {"type": "string", "description": "How late a slot missed while the daemon was down still runs (default 1h)."}
    })
}

/// The MCP annotations the tools below share.
struct Ann {
    ro: Value,
    destructive: Value,
    write: Value,
    // Destinations and backups reach outside isb (S3).
    write_open: Value,
    destructive_open: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
}

/// A command line becomes argv.
fn argv(a: &mut Value) -> Result<()> {
    if let Some(Value::String(line)) = a.get("command") {
        let v =
            crate::flex::split_words(line).map_err(|e| Error::invalid(format!("command: {e}")))?;
        a["command"] = json!(v);
    }
    Ok(())
}

fn job_json(j: &Jobs, org: &OrgId, job: &crate::jobs::Job) -> Value {
    json!({
        "job": job.spec,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "next_run": unix_rfc3339(j.next_run(job)),
        "last_run": j.runs(org, &job.spec.name).last(),
    })
}

pub fn register(r: &mut Registry, ctx: Ctx) -> Result<()> {
    let ann = Ann {
        ro: json!({"readOnlyHint": true, "openWorldHint": false}),
        destructive: json!({"destructiveHint": true, "openWorldHint": false}),
        write: json!({"destructiveHint": false, "openWorldHint": false}),
        write_open: json!({"destructiveHint": false, "openWorldHint": true}),
        destructive_open: json!({"destructiveHint": true, "openWorldHint": true}),
    };

    // --- databases ---------------------------------------------------------

    database_create_tool(r, &ctx, &ann)?;
    database_list_tool(r, &ctx, &ann)?;
    database_get_tool(r, &ctx, &ann)?;

    // --- destinations ------------------------------------------------------

    backup_destination_create_tool(r, &ctx, &ann)?;
    backup_destination_list_tool(r, &ctx, &ann)?;
    backup_destination_delete_tool(r, &ctx, &ann)?;
    backup_destination_test_tool(r, &ctx, &ann)?;

    // --- backups -----------------------------------------------------------

    backup_create_tool(r, &ctx, &ann)?;
    backup_update_tool(r, &ctx, &ann)?;
    backup_list_tool(r, &ctx, &ann)?;
    backup_delete_tool(r, &ctx, &ann)?;
    backup_run_tool(r, &ctx, &ann)?;
    backup_runs_tool(r, &ctx, &ann)?;
    backup_run_log_tool(r, &ctx, &ann)?;
    backup_restore_tool(r, &ctx, &ann)?;

    // --- jobs --------------------------------------------------------------

    job_create_tool(r, &ctx, &ann)?;
    job_list_tool(r, &ctx, &ann)?;
    job_get_tool(r, &ctx, &ann)?;
    job_update_tool(r, &ctx, &ann)?;
    job_delete_tool(r, &ctx, &ann)?;
    job_run_tool(r, &ctx, &ann)?;
    job_runs_tool(r, &ctx, &ann)?;
    job_run_log_tool(r, &ctx, &ann)?;
    Ok(())
}

fn database_create_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "database_create",
        "Create a database",
        "Create a database in a project's environment: Postgres, MySQL, MariaDB, MongoDB or Redis from the official image at `version`, its data on a named volume, one replica rolled out stop-first, with a health check. Credentials are generated and kept as org secrets (db.<name>.password; db.<name>.root-password for MySQL/MariaDB; db.<name>.url, the internal connection URL for apps: DATABASE_URL=${{secret.db.<name>.url}}; `urls` keeps more such secrets with driver options). Setting db.<name>.password changes the password inside the running database first, then the URL secrets. Other apps reach it at <name>.<project>-<env>. Not published outside the org unless `publish` is set. A database is an app: deploy, update, roll back and delete it with the app_* tools.",
        obj(
            json!({
                "name": {"type": "string"},
                "project": {"type": "string"},
                "environment": {"type": "string", "description": "Default production."},
                "engine": {"type": "string", "enum": ["postgres", "mysql", "mariadb", "mongodb", "redis"]},
                "version": {"type": "string", "description": "Image tag (default: 17, 8.4, 11.4, 8.0, 7.4)."},
                "database": {"type": "string", "description": "Database created on first start (default: the name with - as _). Not Redis."},
                "user": {"type": "string", "description": "User created on first start (default: as database). Not Redis."},
                "urls": {"type": "object", "additionalProperties": {"type": "string"}, "description": "More secrets isb keeps holding the internal URL, each with a query string for a driver's options ({\"dsn.main-db.web\": \"sslmode=disable\"}, \"\" for none). Written at deploy and again whenever the password changes, so no app holds a stale copy of it."},
                "publish": {"type": "string", "description": "Publish the port on the host: [IP:]PORT (default address 127.0.0.1). Off by default."},
                "env": {"description": "Extra environment (.env text or a map), e.g. POSTGRES_INITDB_ARGS."},
                "resources": {"type": "object", "description": "{cpus, memory}."},
                "deploy": {"type": "boolean", "description": "Deploy right away (default true)."},
                "wait": {"type": "boolean", "description": "Wait until it is up (default false)."}
            }),
            &["name", "project", "engine"]
        ),
        ann.write,
        |x: &Ctx, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let t = take(
                &mut a,
                &[
                    "org", "engine", "version", "database", "user", "urls", "publish", "deploy",
                    "wait",
                ],
            );
            let engine = crate::app::Engine::parse(
                t.get("engine").and_then(Value::as_str).unwrap_or_default(),
            )?;
            let mut db = json!({"engine": engine});
            for k in ["version", "database", "user", "urls"] {
                if let Some(v) = t.get(k) {
                    db[k] = v.clone();
                }
            }
            a["source"] = json!({"database": db});
            if let Some(p) = t.get("publish").and_then(Value::as_str) {
                let (ip, port) = match p.rsplit_once(':') {
                    Some((ip, port)) => (ip.to_string(), port.to_string()),
                    None => ("127.0.0.1".to_string(), p.to_string()),
                };
                port.parse::<u16>()
                    .map_err(|_| Error::invalid(format!("publish {p:?}: [IP:]PORT")))?;
                a["ports"] = json!([format!("{ip}:{port}:{}", engine.port())]);
            }
            let spec: AppSpec = args(a)?;
            let (app, _) = x.apps.create(&org, spec)?;
            let mut out = json!({"database": database_json(&org, &app, None)});
            if t.get("deploy").and_then(Value::as_bool).unwrap_or(true) {
                let trigger = if c.is_local() {
                    crate::app::deploy::Trigger::Manual
                } else {
                    crate::app::deploy::Trigger::Api
                };
                let d = x
                    .apps
                    .deploy(&org, &app.spec.name, trigger, &caller_name(c), None)?;
                let d = if t.get("wait").and_then(Value::as_bool).unwrap_or(false) {
                    x.apps
                        .wait(&org, &app.spec.name, d.id, Duration::from_secs(900))?
                } else {
                    d
                };
                out["deployment"] = d.summary();
            }
            Ok(out)
        }
    );
    Ok(())
}

fn database_list_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "database_list",
        "List databases",
        "An org's databases (apps with a database source), each with its engine, version, stack and connection details (password as a secret reference).",
        obj(json!({"project": {"type": "string"}}), &[]),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let project = a.get("project").and_then(Value::as_str).map(String::from);
            let dbs: Vec<Value> = x
                .apps
                .list(&org)?
                .into_iter()
                .filter(|d| matches!(d.spec.source, Source::Database(_)))
                .filter(|d| project.as_ref().is_none_or(|p| *p == d.spec.project))
                .map(|d| database_json(&org, &d, None))
                .collect();
            Ok(json!({"databases": dbs}))
        }
    );
    Ok(())
}

fn database_get_tool(r: &mut Registry, ctx: &Ctx, _ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "database_get",
        "Get a database",
        "A database's settings and connection details: host (service name), port, user, database, the password as a reference to its org secret, and a URL with that reference. `reveal: true` adds the password and URL values (org members may read org secrets).",
        obj(
            json!({"name": {"type": "string"}, "reveal": {"type": "boolean"}}),
            &["name"]
        ),
        // `reveal` hands out the password: a secret read (viewers, `read`
        // tokens refused; always audited).
        json!({"readOnlyHint": true, "openWorldHint": false, "isbSecretReadArg": "reveal"}),
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                reveal: bool,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let app = x.apps.get(&org, &a.name)?;
            if !matches!(app.spec.source, Source::Database(_)) {
                return Err(Error::invalid(format!("app {} is not a database", a.name)));
            }
            let pw = if a.reveal {
                let (v, _) = x
                    .apps
                    .secrets()
                    .get(&org, &crate::app::database::password_secret(&a.name))?;
                Some(String::from_utf8_lossy(&v).trim().to_string())
            } else {
                None
            };
            Ok(database_json(&org, &app, pw.as_deref()))
        }
    );
    Ok(())
}

fn backup_destination_create_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_destination_create",
        "Create a backup destination",
        "An S3-compatible bucket for backups: endpoint (https://s3.<region>.amazonaws.com, an R2/B2/MinIO URL), region (default us-east-1), bucket, key prefix, path_style (true for MinIO and most self-hosted stores). The key pair is given as access_key/secret_key (stored as the org secrets backup.<name>.access-key/.secret-key) or as the names of existing secrets. Endpoints on this host (loopback) are for the local CLI and platform admins. create_bucket=true creates the bucket; test=true writes, reads back and deletes a small object.",
        obj(
            json!({
                "name": {"type": "string"},
                "endpoint": {"type": "string"},
                "region": {"type": "string"},
                "bucket": {"type": "string"},
                "prefix": {"type": "string"},
                "path_style": {"type": "boolean"},
                "access_key": {"type": "string"},
                "secret_key": {"type": "string"},
                "access_key_secret": {"type": "string"},
                "secret_key_secret": {"type": "string"},
                "test": {"type": "boolean"},
                "create_bucket": {"type": "boolean", "description": "Create the bucket first (self-hosted stores)."}
            }),
            &["name", "endpoint", "bucket"]
        ),
        ann.write_open,
        |x: &Ctx, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let t = take(
                &mut a,
                &["org", "access_key", "secret_key", "test", "create_bucket"],
            );
            let s = |k: &str| t.get(k).and_then(Value::as_str).map(String::from);
            if let Some(o) = a.as_object_mut() {
                o.entry("access_key_secret").or_insert(json!(""));
                o.entry("secret_key_secret").or_insert(json!(""));
            }
            let d: Destination = args(a)?;
            let d = x.backups.destination_create(
                &org,
                d,
                s("access_key"),
                s("secret_key"),
                trusted(c),
            )?;
            let mut out = json!({"destination": d});
            if t.get("create_bucket")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                x.backups.destination_create_bucket(&org, &d.name)?;
                out["bucket_created"] = json!(true);
            }
            if t.get("test").and_then(Value::as_bool).unwrap_or(false) {
                out["test"] = match x.backups.destination_test(&org, &d.name) {
                    Ok(v) => v,
                    Err(e) => json!({"ok": false, "error": e.to_string()}),
                };
            }
            Ok(out)
        }
    );
    Ok(())
}

fn backup_destination_list_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_destination_list",
        "List backup destinations",
        "An org's backup destinations (key pairs as secret names).",
        obj(json!({}), &[]),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            Ok(json!({"destinations": x.backups.destination_list(&org)?}))
        }
    );
    Ok(())
}

fn backup_destination_delete_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_destination_delete",
        "Delete a backup destination",
        "Delete a destination no backup uses, with the key secrets isb stored for it. Objects in the bucket are kept.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.destructive,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            x.backups.destination_delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

fn backup_destination_test_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_destination_test",
        "Test a backup destination",
        "Write a small object under the destination's prefix, check it with HEAD and delete it.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.write_open,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            x.backups.destination_test(&org, &a.name)
        }
    );
    Ok(())
}

fn backup_create_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_create",
        "Schedule a backup",
        "Back a database (or a named `volume`) up on a cron schedule to a destination. A database: the engine's own dump (pg_dump, mysqldump, mariadb-dump, mongodump, a Redis RDB) runs in the database's instance, is compressed and streamed to the bucket by the daemon, checked with HEAD, and the oldest beyond `keep` are deleted. Emits backup.succeeded / backup.failed events.",
        obj(backup_props(), &["name", "destination", "schedule"]),
        ann.write,
        |x: &Ctx, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            take(&mut a, &["org"]);
            let spec: BackupSpec = args(a)?;
            if spec.volume.is_some() {
                super::volumes::require_admin(c, &org, "backing up a volume")?;
            }
            let b = x.backups.create(&org, spec)?;
            let next = x.backups.next_run(&b);
            Ok(json!({"backup": b, "next_run": unix_rfc3339(next)}))
        }
    );
    Ok(())
}

fn backup_update_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_update",
        "Update a backup",
        "Change a backup's settings (a merge patch: schedule, timezone, destination, keep, compression, enabled, missed_grace). A changed schedule counts from now.",
        obj(backup_props(), &["name"]),
        ann.write,
        |x: &Ctx, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            volume_backup_admin(x, &org, &name, c)?;
            take(&mut a, &["org", "name"]);
            let b = x.backups.update(&org, &name, &a)?;
            let next = x.backups.next_run(&b);
            Ok(json!({"backup": b, "next_run": unix_rfc3339(next)}))
        }
    );
    Ok(())
}

fn backup_list_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_list",
        "List backups",
        "An org's backup schedules with their last run and next run. With `name`, that backup only, plus the backup files in its bucket (newest first): what backup_restore takes.",
        obj(
            json!({"name": {"type": "string"}, "database": {"type": "string"}}),
            &[]
        ),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let name = a.get("name").and_then(Value::as_str);
            let database = a.get("database").and_then(Value::as_str);
            let mut out = Vec::new();
            for b in x.backups.list(&org)? {
                if name.is_some_and(|n| n != b.spec.name)
                    || database.is_some_and(|d| d != b.spec.database)
                {
                    continue;
                }
                let last = x.backups.runs(&org, &b.spec.name).last();
                let next = x.backups.next_run(&b);
                let mut v =
                    json!({"backup": b.spec, "last_run": last, "next_run": unix_rfc3339(next)});
                if name.is_some() {
                    v["files"] = match x.backups.files(&org, &b.spec.name) {
                        Ok(f) => json!(f),
                        Err(e) => json!({"error": e.to_string()}),
                    };
                }
                out.push(v);
            }
            if let (Some(n), true) = (name, out.is_empty()) {
                return Err(Error::NotFound(format!("backup {n} in org {org}")));
            }
            Ok(json!({"backups": out}))
        }
    );
    Ok(())
}

fn backup_delete_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_delete",
        "Delete a backup",
        "Delete a backup schedule and its run records. Its files stay in the bucket (restore them with destination and key).",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.destructive,
        |x: &Ctx, a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            volume_backup_admin(x, &org, &a.name, c)?;
            x.backups.delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

fn backup_run_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_run",
        "Run a backup now",
        "Back up now, outside the schedule. Returns the run; wait=true returns when it finishes (at most `timeout`, default 10m).",
        obj(
            json!({"name": {"type": "string"}, "wait": {"type": "boolean"}, "timeout": {"type": "string"}}),
            &["name"]
        ),
        ann.write_open,
        |x: &Ctx, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                wait: bool,
                #[serde(default)]
                timeout: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            volume_backup_admin(x, &org, &a.name, c)?;
            let r = x.backups.run_now(&org, &a.name, &caller_name(c))?;
            let r = if a.wait {
                wait_run(
                    &x.backups.runs(&org, &a.name),
                    r.id,
                    timeout_arg(&a.timeout, Duration::from_secs(600))?,
                )?
            } else {
                r
            };
            Ok(json!({"run": r}))
        }
    );
    Ok(())
}

fn backup_runs_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_runs",
        "Backup runs",
        "A backup's runs, newest first (status, trigger, duration, object key and size). With restores=true instead, the org's restore runs.",
        obj(
            json!({"name": {"type": "string"}, "restores": {"type": "boolean"}, "limit": {"type": "integer", "minimum": 1, "maximum": 1000}}),
            &[]
        ),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let store = if a.get("restores").and_then(Value::as_bool).unwrap_or(false) {
                x.backups.restore_runs(&org)
            } else {
                let name = a
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("name (or restores: true) is required"))?;
                x.backups.get(&org, name)?;
                x.backups.runs(&org, name)
            };
            Ok(json!({"runs": store.list(limit)}))
        }
    );
    Ok(())
}

fn backup_run_log_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_run_log",
        "Backup run log",
        "One backup (or restore) run's log from byte `offset`; poll with the returned offset until finished.",
        obj(
            json!({
                "name": {"type": "string", "description": "The backup (omit with restore=true)."},
                "restore": {"type": "boolean"},
                "run": {"type": "integer", "minimum": 1},
                "offset": {"type": "integer", "minimum": 0}
            }),
            &["run"]
        ),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let run = a.get("run").and_then(Value::as_u64).unwrap_or(0);
            let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let store = if a.get("restore").and_then(Value::as_bool).unwrap_or(false) {
                x.backups.restore_runs(&org)
            } else {
                let name = a
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("name is required"))?;
                x.backups.get(&org, name)?;
                x.backups.runs(&org, name)
            };
            let (text, offset, finished) = store.log(run, offset)?;
            Ok(
                json!({"text": text, "offset": offset, "finished": finished, "run": store.get(run)?}),
            )
        }
    );
    Ok(())
}

fn backup_restore_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "backup_restore",
        "Restore a backup",
        "Restore a backup file into a database: `backup` (its newest file, or `key`) or `destination` + `key`; into `target`, an existing database of the same engine whose data is REPLACED (needs confirm: true), or `new`: {name, project?, environment?, version?}, a database created for it (by default beside the backed-up one). The file streams from the bucket through the daemon into the engine's restore tool (pg_restore --clean, mysql, mongorestore --drop, a Redis RDB swap). Emits restore.succeeded / restore.failed.",
        obj(
            json!({
                "backup": {"type": "string"},
                "destination": {"type": "string"},
                "key": {"type": "string"},
                "target": {"type": "string"},
                "new": {"type": "object", "properties": {"name": {"type": "string"}, "project": {"type": "string"}, "environment": {"type": "string"}, "version": {"type": "string"}}, "required": ["name"]},
                "confirm": {"type": "boolean"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string"}
            }),
            &[]
        ),
        ann.destructive_open,
        |x: &Ctx, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let t = take(&mut a, &["org", "wait", "timeout"]);
            let req: RestoreRequest = args(a)?;
            let r = x.backups.restore(&org, req, &caller_name(c))?;
            let r = if t.get("wait").and_then(Value::as_bool).unwrap_or(false) {
                let timeout = t.get("timeout").and_then(Value::as_str).map(String::from);
                wait_run(
                    &x.backups.restore_runs(&org),
                    r.id,
                    timeout_arg(&timeout, Duration::from_secs(1800))?,
                )?
            } else {
                r
            };
            Ok(json!({"run": r}))
        }
    );
    Ok(())
}

fn job_create_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_create",
        "Create a scheduled job",
        "Run a command on a cron schedule against an app or a stack service: in a running replica (mode exec) or a fresh one-off instance from its image (mode run). Each run keeps its exit code, duration and output (bounded); job.succeeded / job.failed events.",
        obj(job_props(), &["name", "schedule", "target", "command"]),
        ann.write,
        |x: &Ctx, mut a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            take(&mut a, &["org"]);
            argv(&mut a)?;
            let spec: JobSpec = args(a)?;
            let j = x.jobs.create(&org, spec)?;
            Ok(job_json(&x.jobs, &org, &j))
        }
    );
    Ok(())
}

fn job_list_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_list",
        "List jobs",
        "An org's jobs with their next and last run.",
        obj(json!({}), &[]),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let jobs: Vec<Value> = x
                .jobs
                .list(&org)?
                .iter()
                .map(|j| job_json(&x.jobs, &org, j))
                .collect();
            Ok(json!({"jobs": jobs}))
        }
    );
    Ok(())
}

fn job_get_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_get",
        "Get a job",
        "A job's settings, next run and last run.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            Ok(job_json(&x.jobs, &org, &x.jobs.get(&org, &a.name)?))
        }
    );
    Ok(())
}

fn job_update_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_update",
        "Update a job",
        "Change a job's settings (a merge patch; the name is fixed). A changed schedule counts from now.",
        obj(job_props(), &["name"]),
        ann.write,
        |x: &Ctx, mut a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            take(&mut a, &["org", "name"]);
            argv(&mut a)?;
            let j = x.jobs.update(&org, &name, &a)?;
            Ok(job_json(&x.jobs, &org, &j))
        }
    );
    Ok(())
}

fn job_delete_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_delete",
        "Delete a job",
        "Delete a job and its run records.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.destructive,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            x.jobs.delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

fn job_run_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_run",
        "Run a job now",
        "Run a job now, outside its schedule (refused while a run is going under concurrency skip). wait=true returns when it finishes (at most `timeout`, default 10m).",
        obj(
            json!({"name": {"type": "string"}, "wait": {"type": "boolean"}, "timeout": {"type": "string"}}),
            &["name"]
        ),
        ann.write,
        |x: &Ctx, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                wait: bool,
                #[serde(default)]
                timeout: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let r = x.jobs.run_now(&org, &a.name, &caller_name(c))?;
            let r = if a.wait {
                wait_run(
                    &x.jobs.runs(&org, &a.name),
                    r.id,
                    timeout_arg(&a.timeout, Duration::from_secs(600))?,
                )?
            } else {
                r
            };
            Ok(json!({"run": r}))
        }
    );
    Ok(())
}

fn job_runs_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_runs",
        "Job runs",
        "A job's runs, newest first: trigger (schedule, missed, manual), status (running, succeeded, failed, skipped), exit code, duration, output size.",
        obj(
            json!({"name": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 1000}}),
            &["name"]
        ),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            x.jobs.get(&org, &name)?;
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            Ok(json!({"runs": x.jobs.runs(&org, &name).list(limit)}))
        }
    );
    Ok(())
}

fn job_run_log_tool(r: &mut Registry, ctx: &Ctx, ann: &Ann) -> Result<()> {
    tool!(
        r,
        ctx,
        "job_run_log",
        "Job run log",
        "One run's output from byte `offset` (the first 192 KiB and the last 64 KiB are kept); poll with the returned offset until finished.",
        obj(
            json!({"name": {"type": "string"}, "run": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}),
            &["name", "run"]
        ),
        ann.ro,
        |x: &Ctx, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            x.jobs.get(&org, &name)?;
            let run = a.get("run").and_then(Value::as_u64).unwrap_or(0);
            let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let store = x.jobs.runs(&org, &name);
            let (text, offset, finished) = store.log(run, offset)?;
            Ok(
                json!({"text": text, "offset": offset, "finished": finished, "run": store.get(run)?}),
            )
        }
    );
    Ok(())
}
