//! `isb db ...`, `isb backup ...` and `isb job ...`: the data tools on the
//! local daemon (docs/databases.md, docs/jobs.md).

use std::io::IsTerminal;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use isb::{Error, Result};

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

// --- databases ---------------------------------------------------------------

#[derive(Subcommand)]
pub enum DbCmd {
    /// Create (and deploy) a database in a project's environment.
    Create {
        name: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        environment: Option<String>,
        /// postgres, mysql, mariadb, mongodb or redis, with an optional
        /// image tag: postgres:16 (default per engine: 17, 8.4, 11.4, 8.0,
        /// 7.4).
        #[arg(long)]
        engine: String,
        /// The database created on first start.
        #[arg(long)]
        database: Option<String>,
        #[arg(long)]
        user: Option<String>,
        /// Publish the port on the host: [IP:]PORT (default 127.0.0.1).
        #[arg(long)]
        publish: Option<String>,
        /// Create without deploying.
        #[arg(long)]
        no_deploy: bool,
    },
    /// List databases.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Connection details (the password as a secret reference).
    #[command(alias = "get")]
    Show {
        name: String,
        /// Print the password and URL values too.
        #[arg(long)]
        show_password: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete a database (its volume and credentials are kept).
    #[command(alias = "remove")]
    Rm { name: String },
}

fn print_connection(c: &Value) {
    println!("engine:    {} {}", s(&c["engine"]), s(&c["version"]));
    println!("host:      {}  ({})", s(&c["host"]), s(&c["fqdn"]));
    println!("port:      {}", c["port"]);
    if !c["user"].is_null() {
        println!("user:      {}", s(&c["user"]));
        println!("database:  {}", s(&c["database"]));
    }
    match c["password_value"].as_str() {
        Some(p) => println!("password:  {p}"),
        None => println!(
            "password:  secret {} (isb secret get {} / --show-password)",
            s(&c["password"]["secret"]),
            s(&c["password"]["secret"])
        ),
    }
    println!("url:       {}", s(&c["url_value"]).if_empty(s(&c["url"])));
    println!(
        "for apps:  DATABASE_URL=${{{{secret.{}}}}}",
        s(&c["url_secret"])
    );
    for e in c["external"].as_array().into_iter().flatten() {
        println!("published: {}", s(e));
    }
    println!("volume:    {}", s(&c["volume"]));
}

trait IfEmpty {
    fn if_empty(self, other: String) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, other: String) -> String {
        if self.is_empty() { other } else { self }
    }
}

/// Follow an app deployment (as `isb app deploy` does).
fn follow_deploy(org: &Option<String>, name: &str, id: u64) -> Result<u8> {
    let mut offset = 0u64;
    loop {
        let r = call(
            "app_deployment_log",
            with_org(
                org,
                json!({"name": name, "deployment": id, "offset": offset}),
            ),
            SHORT,
        )?;
        print!("{}", r["log"].as_str().unwrap_or(""));
        offset = r["offset"].as_u64().unwrap_or(offset);
        if r["finished"].as_bool() == Some(true) {
            let st = r["status"].as_str().unwrap_or("");
            eprintln!("deployment {id}: {st}");
            return Ok(if st == "done" { 0 } else { 1 });
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn db(org: &Option<String>, cmd: DbCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        DbCmd::Create {
            name,
            project,
            environment,
            engine,
            database,
            user,
            publish,
            no_deploy,
        } => {
            let (engine, version) = match engine.split_once(':') {
                Some((e, v)) => (e.to_string(), Some(v.to_string())),
                None => (engine, None),
            };
            let mut a =
                json!({"name": name, "project": project, "engine": engine, "deploy": !no_deploy});
            for (k, v) in [
                ("environment", environment),
                ("version", version),
                ("database", database),
                ("user", user),
                ("publish", publish),
            ] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            let r = call("database_create", a)?;
            eprintln!("created database {name}");
            print_connection(&r["database"]["connection"]);
            if let Some(id) = r["deployment"]["id"].as_u64() {
                return follow_deploy(org, &name, id);
            }
        }
        DbCmd::Ls { project, json } => {
            let mut a = json!({});
            if let Some(p) = project {
                a["project"] = json!(p);
            }
            let r = call("database_list", a)?;
            if json {
                print_json(&r["databases"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "ENGINE".into(),
                "VERSION".into(),
                "HOST".into(),
                "PORT".into(),
                "DEPLOYMENT".into(),
            ]];
            for d in r["databases"].as_array().into_iter().flatten() {
                let c = &d["connection"];
                rows.push(vec![
                    s(&d["name"]),
                    s(&c["engine"]),
                    s(&c["version"]),
                    s(&c["host"]),
                    c["port"].to_string(),
                    d["current_deployment"].to_string(),
                ]);
            }
            table(rows);
        }
        DbCmd::Show {
            name,
            show_password,
            json,
        } => {
            let r = call(
                "database_get",
                json!({"name": name, "reveal": show_password}),
            )?;
            if json {
                print_json(&r);
            } else {
                print_connection(&r["connection"]);
            }
        }
        DbCmd::Rm { name } => {
            call("app_delete", json!({"name": name}))?;
            eprintln!(
                "deleted database {name}; its volume and credentials (db.{name}.*) are kept: a database created again with this name reuses them"
            );
        }
    }
    Ok(0)
}

// --- backups -----------------------------------------------------------------

#[derive(Subcommand)]
pub enum DestCmd {
    /// Add an S3-compatible bucket. The secret key is read from
    /// $ISB_S3_SECRET_KEY or, failing that, the first line of stdin.
    Create {
        name: String,
        /// https://s3.<region>.amazonaws.com, an R2/B2/MinIO URL.
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        bucket: String,
        #[arg(long, default_value = "us-east-1")]
        region: String,
        #[arg(long, default_value = "")]
        prefix: String,
        /// endpoint/bucket/key addressing (MinIO and most self-hosted stores).
        #[arg(long)]
        path_style: bool,
        /// The access key id.
        #[arg(long, conflicts_with = "access_key_secret")]
        access_key: Option<String>,
        /// Or: org secrets already holding the key pair.
        #[arg(long, requires = "secret_key_secret")]
        access_key_secret: Option<String>,
        #[arg(long, requires = "access_key_secret")]
        secret_key_secret: Option<String>,
        /// Skip the write/read/delete check.
        #[arg(long)]
        no_test: bool,
        /// Create the bucket (self-hosted stores).
        #[arg(long)]
        create_bucket: bool,
    },
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    #[command(alias = "remove")]
    Rm { name: String },
    /// Write, read back and delete a small object.
    Test { name: String },
}

#[derive(Args)]
pub struct RestoreArgs {
    /// The backup to restore from (its newest file unless --key).
    backup: Option<String>,
    /// Or a destination and --key (a backup deleted since).
    #[arg(long, requires = "key")]
    destination: Option<String>,
    #[arg(long)]
    key: Option<String>,
    /// Restore into this existing database, replacing its data.
    #[arg(long, conflicts_with = "new")]
    into: Option<String>,
    /// Restore into a new database created for it.
    #[arg(long)]
    new: Option<String>,
    /// With --new: its project (default: the backed-up database's).
    #[arg(long, requires = "new")]
    project: Option<String>,
    #[arg(long, requires = "new")]
    environment: Option<String>,
    /// Do not ask before replacing data.
    #[arg(short, long)]
    yes: bool,
    /// Return once started.
    #[arg(short, long)]
    detach: bool,
}

#[derive(Subcommand)]
pub enum BackupCmd {
    /// Backup destinations (S3-compatible buckets).
    #[command(subcommand, alias = "destination")]
    Dest(DestCmd),
    /// Back a database (or a named volume) up on a schedule.
    Create {
        name: String,
        #[arg(long, required_unless_present = "volume", conflicts_with = "volume")]
        database: Option<String>,
        /// A named volume in the org instead (restore with `isb volume restore`).
        #[arg(long)]
        volume: Option<String>,
        #[arg(long)]
        destination: String,
        /// Cron (five fields) or @hourly, @daily, @weekly, ...
        #[arg(long)]
        schedule: String,
        /// UTC (default) or a fixed offset like +02:00.
        #[arg(long)]
        timezone: Option<String>,
        /// Backups kept in the bucket (default 7).
        #[arg(long)]
        keep: Option<u32>,
        /// gzip (default), zstd or none.
        #[arg(long)]
        compression: Option<String>,
    },
    /// Change a backup's schedule, keep, destination; enable or disable it.
    Update {
        name: String,
        #[arg(long)]
        schedule: Option<String>,
        #[arg(long)]
        keep: Option<u32>,
        #[arg(long)]
        destination: Option<String>,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
    },
    /// List backups with their last and next runs.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// The files a backup has in its bucket, newest first.
    Files {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Back up now and follow it.
    Run {
        name: String,
        #[arg(short, long)]
        detach: bool,
    },
    /// A backup's runs (or, with --restores, the org's restores).
    Runs {
        name: Option<String>,
        #[arg(long)]
        restores: bool,
        #[arg(long)]
        json: bool,
    },
    /// A run's log (default: the latest); -f follows it.
    Logs {
        name: Option<String>,
        run: Option<u64>,
        /// A restore run's log.
        #[arg(long)]
        restore: bool,
        #[arg(short, long)]
        follow: bool,
    },
    /// Restore a backup into a database.
    Restore(RestoreArgs),
    /// Delete a backup schedule (its files stay in the bucket).
    #[command(alias = "remove")]
    Rm { name: String },
}

/// Print a run's log as it grows: 0 when it succeeded.
fn follow_run(
    org: &Option<String>,
    tool: &str,
    mut args: Value,
    id: u64,
    what: &str,
) -> Result<u8> {
    let mut offset = 0u64;
    args["run"] = json!(id);
    loop {
        args["offset"] = json!(offset);
        let r = call(tool, with_org(org, args.clone()), SHORT)?;
        print!("{}", r["text"].as_str().unwrap_or(""));
        offset = r["offset"].as_u64().unwrap_or(offset);
        if r["finished"].as_bool() == Some(true) {
            let run = &r["run"];
            let st = s(&run["status"]);
            let mut line = format!("{what} run {id}: {st}");
            if let Some(e) = run["error"].as_str() {
                line.push_str(&format!(" ({e})"));
            }
            if let Some(k) = run["detail"]["key"].as_str() {
                line.push_str(&format!(" {k}"));
            }
            eprintln!("{line}");
            return Ok(if st == "succeeded" { 0 } else { 1 });
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn runs_table(runs: &Value) {
    let mut rows = vec![vec![
        "RUN".into(),
        "STATUS".into(),
        "TRIGGER".into(),
        "STARTED".into(),
        "SECONDS".into(),
        "EXIT".into(),
        "DETAIL".into(),
    ]];
    for r in runs.as_array().into_iter().flatten() {
        let started = r["started_at"]
            .as_u64()
            .map(|ms| isb::cron::rfc3339((ms / 1000) as i64))
            .unwrap_or_default();
        let secs = r["duration_ms"]
            .as_u64()
            .map(|ms| format!("{:.1}", ms as f64 / 1000.0))
            .unwrap_or_default();
        let detail = match (r["detail"]["key"].as_str(), r["error"].as_str()) {
            (Some(k), _) => format!(
                "{k} ({} bytes)",
                s(&r["detail"]["size"]).if_empty(s(&r["detail"]["bytes"]))
            ),
            (None, Some(e)) => e.to_string(),
            _ => String::new(),
        };
        rows.push(vec![
            r["id"].to_string(),
            s(&r["status"]),
            s(&r["trigger"]),
            started,
            secs,
            s(&r["exit_code"]),
            detail,
        ]);
    }
    table(rows);
}

/// Ask on the terminal; refuse without one.
fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::Invalid(format!("{question}: pass --yes to confirm")));
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn backup(org: &Option<String>, cmd: BackupCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        BackupCmd::Dest(d) => match d {
            DestCmd::Create {
                name,
                endpoint,
                bucket,
                region,
                prefix,
                path_style,
                access_key,
                access_key_secret,
                secret_key_secret,
                no_test,
                create_bucket,
            } => {
                let mut a = json!({
                    "name": name, "endpoint": endpoint, "bucket": bucket, "region": region,
                    "prefix": prefix, "path_style": path_style, "test": !no_test,
                    "create_bucket": create_bucket,
                });
                match (access_key, access_key_secret, secret_key_secret) {
                    (_, Some(ak), Some(sk)) => {
                        a["access_key_secret"] = json!(ak);
                        a["secret_key_secret"] = json!(sk);
                    }
                    (Some(ak), _, _) => {
                        let sk = match std::env::var("ISB_S3_SECRET_KEY") {
                            Ok(v) if !v.is_empty() => v,
                            _ => {
                                if std::io::stdin().is_terminal() {
                                    eprint!("secret key: ");
                                }
                                let mut l = String::new();
                                std::io::stdin().read_line(&mut l)?;
                                l.trim().to_string()
                            }
                        };
                        if sk.is_empty() {
                            return Err(Error::Invalid("no secret key given".into()));
                        }
                        a["access_key"] = json!(ak);
                        a["secret_key"] = json!(sk);
                    }
                    _ => {
                        return Err(Error::Invalid(
                            "give --access-key (secret key from $ISB_S3_SECRET_KEY or stdin), or --access-key-secret and --secret-key-secret".into(),
                        ));
                    }
                }
                let r = call("backup_destination_create", a)?;
                eprintln!("created destination {name}");
                if !r["test"].is_null() {
                    if r["test"]["ok"].as_bool() == Some(true) {
                        eprintln!("test: ok ({} ms)", r["test"]["ms"]);
                    } else {
                        eprintln!("test: FAILED: {}", s(&r["test"]["error"]));
                        return Ok(1);
                    }
                }
            }
            DestCmd::Ls { json } => {
                let r = call("backup_destination_list", json!({}))?;
                if json {
                    print_json(&r["destinations"]);
                    return Ok(0);
                }
                let mut rows = vec![vec![
                    "NAME".into(),
                    "ENDPOINT".into(),
                    "BUCKET".into(),
                    "PREFIX".into(),
                    "KEYS".into(),
                ]];
                for d in r["destinations"].as_array().into_iter().flatten() {
                    rows.push(vec![
                        s(&d["name"]),
                        s(&d["endpoint"]),
                        s(&d["bucket"]),
                        s(&d["prefix"]),
                        format!(
                            "{}, {}",
                            s(&d["access_key_secret"]),
                            s(&d["secret_key_secret"])
                        ),
                    ]);
                }
                table(rows);
            }
            DestCmd::Rm { name } => {
                call("backup_destination_delete", json!({"name": name}))?;
            }
            DestCmd::Test { name } => {
                let r = call("backup_destination_test", json!({"name": name}))?;
                eprintln!("ok ({} ms)", r["ms"]);
            }
        },
        BackupCmd::Create {
            name,
            database,
            volume,
            destination,
            schedule,
            timezone,
            keep,
            compression,
        } => {
            let mut a = json!({"name": name, "destination": destination, "schedule": schedule});
            match (database, volume) {
                (Some(d), _) => a["database"] = json!(d),
                (None, Some(v)) => a["volume"] = json!(v),
                (None, None) => {}
            }
            if let Some(t) = timezone {
                a["timezone"] = json!(t);
            }
            if let Some(k) = keep {
                a["keep"] = json!(k);
            }
            if let Some(c) = compression {
                a["compression"] = json!(c);
            }
            let r = call("backup_create", a)?;
            eprintln!("created backup {name}; next run {}", s(&r["next_run"]));
        }
        BackupCmd::Update {
            name,
            schedule,
            keep,
            destination,
            enable,
            disable,
        } => {
            let mut a = json!({"name": name});
            if let Some(v) = schedule {
                a["schedule"] = json!(v);
            }
            if let Some(v) = keep {
                a["keep"] = json!(v);
            }
            if let Some(v) = destination {
                a["destination"] = json!(v);
            }
            if enable || disable {
                a["enabled"] = json!(enable);
            }
            let r = call("backup_update", a)?;
            eprintln!(
                "updated backup {name}; next run {}",
                s(&r["next_run"]).if_empty("never".into())
            );
        }
        BackupCmd::Ls { json } => {
            let r = call("backup_list", json!({}))?;
            if json {
                print_json(&r["backups"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "SOURCE".into(),
                "DESTINATION".into(),
                "SCHEDULE".into(),
                "KEEP".into(),
                "LAST".into(),
                "NEXT".into(),
            ]];
            for b in r["backups"].as_array().into_iter().flatten() {
                let spec = &b["backup"];
                let mut sched = s(&spec["schedule"]);
                if spec["enabled"] == json!(false) {
                    sched.push_str(" (disabled)");
                }
                rows.push(vec![
                    s(&spec["name"]),
                    match spec["volume"].as_str() {
                        Some(v) => format!("volume {v}"),
                        None => s(&spec["database"]),
                    },
                    s(&spec["destination"]),
                    sched,
                    spec["keep"].to_string(),
                    s(&b["last_run"]["status"]),
                    s(&b["next_run"]),
                ]);
            }
            table(rows);
        }
        BackupCmd::Files { name, json } => {
            let r = call("backup_list", json!({"name": name}))?;
            let files = &r["backups"][0]["files"];
            if let Some(e) = files["error"].as_str() {
                return Err(Error::Invalid(e.to_string()));
            }
            if json {
                print_json(files);
                return Ok(0);
            }
            let mut rows = vec![vec!["TAKEN".into(), "SIZE".into(), "KEY".into()]];
            for f in files.as_array().into_iter().flatten() {
                rows.push(vec![s(&f["taken_at"]), f["size"].to_string(), s(&f["key"])]);
            }
            table(rows);
        }
        BackupCmd::Run { name, detach } => {
            let r = call("backup_run", json!({"name": name}))?;
            let id = r["run"]["id"].as_u64().unwrap_or(0);
            eprintln!("backup {name}: run {id} started");
            if !detach {
                return follow_run(org, "backup_run_log", json!({"name": name}), id, "backup");
            }
        }
        BackupCmd::Runs {
            name,
            restores,
            json,
        } => {
            let mut a = json!({"restores": restores});
            if let Some(n) = name {
                a["name"] = json!(n);
            }
            let r = call("backup_runs", a)?;
            if json {
                print_json(&r["runs"]);
            } else {
                runs_table(&r["runs"]);
            }
        }
        BackupCmd::Logs {
            name,
            run,
            restore,
            follow,
        } => {
            let mut a = json!({"restore": restore});
            if let Some(n) = &name {
                a["name"] = json!(n);
            }
            let id = match run {
                Some(r) => r,
                None => {
                    let mut q = json!({"restores": restore, "limit": 1});
                    if let Some(n) = &name {
                        q["name"] = json!(n);
                    }
                    call("backup_runs", q)?["runs"][0]["id"]
                        .as_u64()
                        .ok_or_else(|| Error::Invalid("no runs yet".into()))?
                }
            };
            if follow {
                return follow_run(
                    org,
                    "backup_run_log",
                    a,
                    id,
                    if restore { "restore" } else { "backup" },
                );
            }
            a["run"] = json!(id);
            print!(
                "{}",
                call("backup_run_log", a)?["text"].as_str().unwrap_or("")
            );
        }
        BackupCmd::Restore(r) => {
            let mut a = json!({});
            if let Some(b) = &r.backup {
                a["backup"] = json!(b);
            }
            if let Some(d) = &r.destination {
                a["destination"] = json!(d);
            }
            if let Some(k) = &r.key {
                a["key"] = json!(k);
            }
            match (&r.into, &r.new) {
                (Some(t), None) => {
                    if !r.yes
                        && !confirm(&format!(
                            "restore {} into {t}, replacing its data?",
                            r.key.as_deref().unwrap_or("the newest backup")
                        ))?
                    {
                        eprintln!("not restored");
                        return Ok(1);
                    }
                    a["target"] = json!(t);
                    a["confirm"] = json!(true);
                }
                (None, Some(n)) => {
                    let mut new = json!({"name": n});
                    if let Some(p) = &r.project {
                        new["project"] = json!(p);
                    }
                    if let Some(e) = &r.environment {
                        new["environment"] = json!(e);
                    }
                    a["new"] = new;
                }
                _ => {
                    return Err(Error::Invalid(
                        "restore --into DATABASE (replacing its data) or --new NAME".into(),
                    ));
                }
            }
            let run = call("backup_restore", a)?;
            let id = run["run"]["id"].as_u64().unwrap_or(0);
            eprintln!(
                "restore {id}: {} into {}",
                s(&run["run"]["detail"]["key"]),
                s(&run["run"]["detail"]["target"])
            );
            if !r.detach {
                return follow_run(
                    org,
                    "backup_run_log",
                    json!({"restore": true}),
                    id,
                    "restore",
                );
            }
        }
        BackupCmd::Rm { name } => {
            call("backup_delete", json!({"name": name}))?;
        }
    }
    Ok(0)
}

// --- jobs --------------------------------------------------------------------

#[derive(Subcommand)]
pub enum JobCmd {
    /// Run a command on a schedule: isb job create NAME --schedule CRON --app APP -- CMD...
    Create {
        name: String,
        /// Cron (five fields) or @hourly, @daily, ...
        #[arg(long)]
        schedule: String,
        #[arg(long, conflicts_with_all = ["stack", "service"])]
        app: Option<String>,
        #[arg(long, requires = "service")]
        stack: Option<String>,
        #[arg(long, requires = "stack")]
        service: Option<String>,
        /// exec (in a running replica, default) or run (a fresh one-off
        /// instance from the service's image).
        #[arg(long)]
        mode: Option<String>,
        /// Kill after this long (default 10m).
        #[arg(long)]
        timeout: Option<String>,
        /// skip (default) or allow overlapping runs.
        #[arg(long)]
        concurrency: Option<String>,
        /// Runs kept (default 20).
        #[arg(long)]
        keep: Option<u32>,
        /// UTC (default) or a fixed offset like +02:00.
        #[arg(long)]
        timezone: Option<String>,
        #[arg(short, long)]
        user: Option<String>,
        /// KEY=VALUE (repeatable).
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// The command (argv; run `sh -c '...'` for a shell line).
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    #[command(alias = "get")]
    Show { name: String },
    /// Change a job's schedule, timeout or command; enable or disable it.
    Update {
        name: String,
        #[arg(long)]
        schedule: Option<String>,
        #[arg(long)]
        timeout: Option<String>,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        /// A new command.
        #[arg(last = true)]
        command: Vec<String>,
    },
    #[command(alias = "remove")]
    Rm { name: String },
    /// Run now and follow it.
    Run {
        name: String,
        #[arg(short, long)]
        detach: bool,
    },
    /// A job's runs, newest first.
    Runs {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// A run's output (default: the latest); -f follows it.
    Logs {
        name: String,
        run: Option<u64>,
        #[arg(short, long)]
        follow: bool,
    },
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn job(org: &Option<String>, cmd: JobCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        JobCmd::Create {
            name,
            schedule,
            app,
            stack,
            service,
            mode,
            timeout,
            concurrency,
            keep,
            timezone,
            user,
            env,
            command,
        } => {
            let target = match (app, stack, service) {
                (Some(a), _, _) => json!({"app": a}),
                (None, Some(st), Some(sv)) => json!({"stack": st, "service": sv}),
                _ => {
                    return Err(Error::Invalid(
                        "give --app, or --stack and --service".into(),
                    ));
                }
            };
            let mut a =
                json!({"name": name, "schedule": schedule, "target": target, "command": command});
            for (k, v) in [
                ("mode", mode),
                ("timeout", timeout),
                ("concurrency", concurrency),
                ("timezone", timezone),
                ("user", user),
            ] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            if let Some(k) = keep {
                a["keep"] = json!(k);
            }
            if !env.is_empty() {
                let mut m = serde_json::Map::new();
                for e in &env {
                    let (k, v) = e
                        .split_once('=')
                        .ok_or_else(|| Error::Invalid(format!("--env {e:?}: KEY=VALUE")))?;
                    m.insert(k.into(), json!(v));
                }
                a["env"] = Value::Object(m);
            }
            let r = call("job_create", a)?;
            eprintln!("created job {name}; next run {}", s(&r["next_run"]));
        }
        JobCmd::Ls { json } => {
            let r = call("job_list", json!({}))?;
            if json {
                print_json(&r["jobs"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "SCHEDULE".into(),
                "TARGET".into(),
                "MODE".into(),
                "COMMAND".into(),
                "LAST".into(),
                "NEXT".into(),
            ]];
            for j in r["jobs"].as_array().into_iter().flatten() {
                let spec = &j["job"];
                let t = &spec["target"];
                let target = match t["app"].as_str() {
                    Some(a) => format!("app {a}"),
                    None => format!("{}/{}", s(&t["stack"]), s(&t["service"])),
                };
                let mut sched = s(&spec["schedule"]);
                if spec["enabled"] == json!(false) {
                    sched.push_str(" (disabled)");
                }
                let cmd: Vec<String> = spec["command"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(s)
                    .collect();
                let last = match j["last_run"]["status"].as_str() {
                    Some(st) => format!("{st} #{}", j["last_run"]["id"]),
                    None => String::new(),
                };
                rows.push(vec![
                    s(&spec["name"]),
                    sched,
                    target,
                    s(&spec["mode"]),
                    cmd.join(" "),
                    last,
                    s(&j["next_run"]),
                ]);
            }
            table(rows);
        }
        JobCmd::Show { name } => {
            print_json(&call("job_get", json!({"name": name}))?);
        }
        JobCmd::Update {
            name,
            schedule,
            timeout,
            enable,
            disable,
            command,
        } => {
            let mut a = json!({"name": name});
            if let Some(v) = schedule {
                a["schedule"] = json!(v);
            }
            if let Some(v) = timeout {
                a["timeout"] = json!(v);
            }
            if enable || disable {
                a["enabled"] = json!(enable);
            }
            if !command.is_empty() {
                a["command"] = json!(command);
            }
            let r = call("job_update", a)?;
            eprintln!(
                "updated job {name}; next run {}",
                s(&r["next_run"]).if_empty("never".into())
            );
        }
        JobCmd::Rm { name } => {
            call("job_delete", json!({"name": name}))?;
        }
        JobCmd::Run { name, detach } => {
            let r = call("job_run", json!({"name": name}))?;
            let id = r["run"]["id"].as_u64().unwrap_or(0);
            eprintln!("job {name}: run {id} started");
            if !detach {
                return follow_run(org, "job_run_log", json!({"name": name}), id, "job");
            }
        }
        JobCmd::Runs { name, json } => {
            let r = call("job_runs", json!({"name": name}))?;
            if json {
                print_json(&r["runs"]);
            } else {
                runs_table(&r["runs"]);
            }
        }
        JobCmd::Logs { name, run, follow } => {
            let id = match run {
                Some(r) => r,
                None => call("job_runs", json!({"name": name, "limit": 1}))?["runs"][0]["id"]
                    .as_u64()
                    .ok_or_else(|| Error::Invalid(format!("job {name} has not run yet")))?,
            };
            if follow {
                return follow_run(org, "job_run_log", json!({"name": name}), id, "job");
            }
            print!(
                "{}",
                call("job_run_log", json!({"name": name, "run": id}))?["text"]
                    .as_str()
                    .unwrap_or("")
            );
        }
    }
    Ok(0)
}
