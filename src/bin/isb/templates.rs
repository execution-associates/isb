//! `isb template ...`: the template tools on the local daemon.

use std::time::Duration;

use clap::Subcommand;
use serde_json::{Value, json};

use isb::{Error, Result};

use super::{SHORT, call, print_json, table};

#[derive(Subcommand)]
pub enum TemplateCmd {
    /// List templates (the built-in catalog and any added ones).
    #[command(alias = "list")]
    Ls {
        /// Words to look for in the name, description and tags.
        query: Vec<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        catalog: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// A template's variables, apps and notes (and, for a Dokploy or
    /// Coolify template, how its translation went).
    #[command(alias = "get")]
    Show {
        /// catalog/id, or a bare id.
        template: String,
        #[arg(long)]
        json: bool,
    },
    /// Deploy a template into a project environment as apps.
    Deploy {
        template: String,
        #[arg(long)]
        project: String,
        /// Default: production.
        #[arg(long = "env")]
        environment: Option<String>,
        /// Names the apps and secrets (default: the template id).
        #[arg(long)]
        name: Option<String>,
        /// A variable, KEY=VALUE (repeatable).
        #[arg(long = "set", short = 's')]
        set: Vec<String>,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Return once the apps are created; they deploy in the background.
        #[arg(short, long)]
        detach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Deployed templates in the org.
    Instances {
        #[arg(long)]
        json: bool,
    },
    /// Delete a deployed template: its apps (volumes are kept) and secrets.
    #[command(alias = "remove")]
    Rm { name: String },
    /// Catalogs added to the built-in one (adding and removing: platform
    /// admins).
    #[command(subcommand)]
    Catalog(CatalogCmd),
}

#[derive(Subcommand)]
pub enum CatalogCmd {
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Add a catalog: a host directory or an https URL.
    Add {
        name: String,
        /// native (isb templates), dokploy or coolify.
        #[arg(long, default_value = "native")]
        format: String,
        location: String,
    },
    #[command(alias = "remove")]
    Rm { name: String },
}

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
        other => other.to_string(),
    }
}

#[expect(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn template(org: &Option<String>, cmd: TemplateCmd) -> Result<u8> {
    let call_t = |tool: &str, args: Value, t: Duration| call(tool, with_org(org, args), t);
    match cmd {
        TemplateCmd::Ls {
            query,
            tag,
            catalog,
            json,
        } => {
            let r = call_t(
                "template_list",
                json!({"query": query.join(" "), "tag": tag, "catalog": catalog}),
                SHORT,
            )?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "REF".into(),
                "NAME".into(),
                "TAGS".into(),
                "DESCRIPTION".into(),
            ]];
            for t in r["templates"].as_array().cloned().unwrap_or_default() {
                let mut d = s(&t["description"]);
                if d.chars().count() > 70 {
                    d = d.chars().take(67).collect::<String>() + "...";
                }
                rows.push(vec![
                    s(&t["ref"]),
                    s(&t["name"]),
                    t["tags"]
                        .as_array()
                        .map(|a| a.iter().map(s).collect::<Vec<_>>().join(","))
                        .unwrap_or_default(),
                    d,
                ]);
            }
            table(rows);
            for e in r["errors"].as_array().cloned().unwrap_or_default() {
                eprintln!("warning: {}", s(&e));
            }
        }
        TemplateCmd::Show { template, json } => {
            let r = call_t("template_get", json!({"template": template}), SHORT)?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            let t = &r["template"];
            println!("{} ({})", s(&t["name"]), s(&t["ref"]));
            println!("{}", s(&t["description"]));
            if let Some(c) = r.get("compatibility") {
                println!(
                    "\n{} translation: {}",
                    match s(&t["format"]).as_str() {
                        "coolify" => "Coolify",
                        _ => "Dokploy",
                    },
                    s(&c["status"])
                );
                for x in c["refusals"].as_array().cloned().unwrap_or_default() {
                    println!("  refused: {}", s(&x));
                }
                for x in c["notes"].as_array().cloned().unwrap_or_default() {
                    println!("  note: {}", s(&x));
                }
            }
            if let Some(vars) = r["variables"].as_array() {
                println!("\nVariables:");
                let mut rows = vec![vec![
                    "NAME".into(),
                    "TYPE".into(),
                    "DEFAULT".into(),
                    "".into(),
                    "DESCRIPTION".into(),
                ]];
                for v in vars {
                    let flag = if v["required"] == json!(true) {
                        "required"
                    } else if v["generated"] == json!(true) {
                        "generated"
                    } else {
                        ""
                    };
                    rows.push(vec![
                        s(&v["name"]),
                        if v["type"].is_null() {
                            "string".into()
                        } else {
                            s(&v["type"])
                        },
                        s(&v["default"]),
                        flag.into(),
                        s(&v["description"]),
                    ]);
                }
                table(rows);
            }
            if let Some(apps) = r["apps"].as_array() {
                println!("\nApps:");
                for a in apps {
                    println!("  {}: {}", s(&a["key"]), s(&a["image"]));
                }
            }
            for n in r["notes"].as_array().cloned().unwrap_or_default() {
                println!("note: {}", s(&n));
            }
        }
        TemplateCmd::Deploy {
            template,
            project,
            environment,
            name,
            set,
            dry_run,
            detach,
            json,
        } => {
            let mut values = serde_json::Map::new();
            for kv in set {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| Error::Invalid(format!("--set {kv}: KEY=VALUE")))?;
                values.insert(k.to_string(), json!(v));
            }
            let args = json!({
                "template": template, "project": project, "environment": environment,
                "name": name, "values": values, "dry_run": dry_run, "wait": !dry_run && !detach,
            });
            let r = call_t("template_deploy", args, Duration::from_secs(3 * 3600))?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            let p = &r["plan"];
            println!(
                "{} {} as {} in {}/{} (stack {})",
                if dry_run { "would deploy" } else { "deploying" },
                s(&r["ref"]),
                s(&p["instance"]),
                s(&p["project"]),
                s(&p["environment"]),
                s(&p["stack"])
            );
            for a in p["apps"].as_array().cloned().unwrap_or_default() {
                println!("  app {}: {}", s(&a["name"]), s(&a["source"]["image"]));
            }
            for x in p["secrets"].as_array().cloned().unwrap_or_default() {
                println!("  secret {} ({})", s(&x["name"]), s(&x["holds"]));
            }
            for v in p["variables"].as_array().cloned().unwrap_or_default() {
                if !v["value"].is_null() {
                    println!(
                        "  {} = {} ({})",
                        s(&v["name"]),
                        s(&v["value"]),
                        s(&v["source"])
                    );
                }
            }
            for u in p["urls"].as_array().cloned().unwrap_or_default() {
                println!("  url {}", s(&u));
            }
            for n in p["notes"].as_array().cloned().unwrap_or_default() {
                println!("note: {}", s(&n));
            }
            if let Some(e) = r.get("error") {
                eprintln!("isb: {}", s(e));
                return Ok(1);
            }
            let mut ok = true;
            for d in r["deployments"].as_array().cloned().unwrap_or_default() {
                let st = s(&d["deployment"]["status"]);
                println!(
                    "{}: {}{}",
                    s(&d["app"]),
                    if st.is_empty() { "error" } else { &st },
                    d.get("error")
                        .or(d["deployment"].get("error"))
                        .map(|e| format!(": {}", s(e)))
                        .unwrap_or_default()
                );
                ok &= st == "done";
            }
            if let Some(names) = r["deploying"].as_array() {
                println!(
                    "deploying in the background, in order: {} (isb app deployments NAME)",
                    names.iter().map(s).collect::<Vec<_>>().join(", ")
                );
            }
            return Ok(if ok { 0 } else { 1 });
        }
        TemplateCmd::Instances { json } => {
            let r = call_t("template_instance_list", json!({}), SHORT)?;
            if json {
                print_json(&r["instances"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "TEMPLATE".into(),
                "PROJECT/ENV".into(),
                "APPS".into(),
                "URLS".into(),
            ]];
            for i in r["instances"].as_array().cloned().unwrap_or_default() {
                rows.push(vec![
                    s(&i["name"]),
                    s(&i["template"]),
                    format!("{}/{}", s(&i["project"]), s(&i["environment"])),
                    i["apps"]
                        .as_array()
                        .map(|a| a.iter().map(s).collect::<Vec<_>>().join(","))
                        .unwrap_or_default(),
                    i["urls"]
                        .as_array()
                        .map(|a| a.iter().map(s).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default(),
                ]);
            }
            table(rows);
        }
        TemplateCmd::Rm { name } => {
            let r = call_t(
                "template_instance_delete",
                json!({"name": name}),
                Duration::from_secs(900),
            )?;
            println!(
                "removed apps {}; secrets {}",
                r["apps"]
                    .as_array()
                    .map(|a| a.iter().map(s).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default(),
                r["secrets"]
                    .as_array()
                    .map(|a| a.iter().map(s).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            );
        }
        TemplateCmd::Catalog(c) => match c {
            CatalogCmd::Ls { json } => {
                let r = call_t("template_catalog_list", json!({}), SHORT)?;
                if json {
                    print_json(&r["catalogs"]);
                    return Ok(0);
                }
                let mut rows = vec![vec!["NAME".into(), "FORMAT".into(), "LOCATION".into()]];
                rows.push(vec!["builtin".into(), "native".into(), "(in isb)".into()]);
                for c in r["catalogs"].as_array().cloned().unwrap_or_default() {
                    rows.push(vec![s(&c["name"]), s(&c["format"]), s(&c["location"])]);
                }
                table(rows);
            }
            CatalogCmd::Add {
                name,
                format,
                location,
            } => {
                call_t(
                    "template_catalog_add",
                    json!({"name": name, "format": format, "location": location}),
                    SHORT,
                )?;
                println!("added catalog {name}");
            }
            CatalogCmd::Rm { name } => {
                call_t("template_catalog_remove", json!({"name": name}), SHORT)?;
                println!("removed catalog {name}");
            }
        },
    }
    Ok(0)
}
