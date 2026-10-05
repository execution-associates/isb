//! `isb db`: databases as apps (docs/guides/databases.md).

use super::*;

#[derive(Subcommand)]
pub enum DbCmd {
    /// Create (and deploy) a database in a project's environment.
    Create(Box<DbCreate>),
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

#[derive(clap::Args)]
pub struct DbCreate {
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
    /// Another secret isb keeps holding the internal URL, with an
    /// optional query string for driver options:
    /// `dsn.main-db.web?sslmode=disable`. Repeatable.
    #[arg(long = "url", value_name = "SECRET[?QUERY]")]
    urls: Vec<String>,
    /// CPUs, e.g. 2.
    #[arg(long)]
    cpus: Option<String>,
    /// Memory: 512m, 2g, 2GiB.
    #[arg(long)]
    memory: Option<String>,
    /// Create without deploying.
    #[arg(long)]
    no_deploy: bool,
}

pub fn db(org: &Option<String>, cmd: DbCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        DbCmd::Create(c) => {
            let DbCreate {
                name,
                project,
                environment,
                engine,
                database,
                user,
                publish,
                urls,
                cpus,
                memory,
                no_deploy,
            } = *c;
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
            if !urls.is_empty() {
                a["urls"] = url_map(&urls);
            }
            if let Some(r) = crate::apps::resources_arg(&cpus, &memory) {
                a["resources"] = r;
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

/// `--url SECRET[?QUERY]` flags as `database_create`'s `urls` map.
fn url_map(urls: &[String]) -> Value {
    urls.iter()
        .map(|u| {
            let (n, q) = u.split_once('?').unwrap_or((u, ""));
            (n.to_string(), json!(q))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}
