//! `isb workspace image ...`: workspace images built from recipe scripts on
//! the `isb serve` host (docs/guides/workspace-images.md).

use clap::Subcommand;
use serde_json::{Value, json};

use isb::Result;

use super::super::{SHORT, call, print_json, table};
use super::{ago, opt, read_file};

#[derive(Subcommand)]
pub enum ImageCmd {
    /// Build an image from a recipe script, following its log. Without
    /// --recipe, isb's default recipe (as isb-workspace).
    Build {
        /// The local image alias (default isb-workspace).
        name: Option<String>,
        /// The recipe: a shell script run as root (a file, or - for stdin).
        #[arg(long, value_name = "FILE")]
        recipe: Option<String>,
        /// The image it starts from (default images:ubuntu/24.04).
        #[arg(long)]
        base: Option<String>,
        /// The description the create form shows.
        #[arg(long)]
        description: Option<String>,
        /// Longest the recipe may run (default 30m, at most 2h).
        #[arg(long)]
        timeout: Option<String>,
        /// Build even when the image is up to date.
        #[arg(long)]
        force: bool,
        /// Print the build's id and return.
        #[arg(long)]
        no_follow: bool,
    },
    /// The images isb built on this host, and recent builds.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Follow a build started elsewhere.
    Logs { id: String },
    /// Remove an image isb built (never one it did not).
    #[command(alias = "remove")]
    Rm { name: String },
}

/// Print a build's log until it ends; the exit status says how.
fn follow(id: &str) -> Result<u8> {
    let mut since = 0u64;
    loop {
        let r = call(
            "workspace_image_logs",
            json!({"id": id, "since": since, "wait": 20}),
            SHORT,
        )?;
        for l in r["lines"].as_array().into_iter().flatten() {
            eprintln!("{}", l.as_str().unwrap_or_default());
        }
        since = r["next"].as_u64().unwrap_or(since);
        match r["state"].as_str() {
            Some("succeeded") => {
                let i = &r["image"];
                let mb = i["size"].as_u64().map(|b| format!(", {} MiB", b >> 20));
                println!(
                    "{} {}{}",
                    i["name"].as_str().unwrap_or_default(),
                    if i["up_to_date"] == true {
                        "is up to date".to_string()
                    } else {
                        format!("built in {}s", i["seconds"].as_u64().unwrap_or(0))
                    },
                    mb.unwrap_or_default()
                );
                return Ok(0);
            }
            Some("failed") => {
                eprintln!(
                    "isb: the image was not built: {}",
                    r["error"].as_str().unwrap_or("see the log")
                );
                return Ok(1);
            }
            _ => {}
        }
    }
}

fn list(json: bool) -> Result<u8> {
    let v = call("workspace_image_list", json!({}), SHORT)?;
    if json {
        print_json(&v);
        return Ok(0);
    }
    let mut rows = vec![vec![
        "NAME".into(),
        "SIZE".into(),
        "BASE".into(),
        "BUILT".into(),
        "BY".into(),
        "DESCRIPTION".into(),
    ]];
    for i in v["images"].as_array().into_iter().flatten() {
        rows.push(vec![
            i["name"].as_str().unwrap_or("-").into(),
            i["size"]
                .as_u64()
                .map(|b| format!("{} MiB", b >> 20))
                .unwrap_or_else(|| "-".into()),
            i["base"].as_str().unwrap_or("-").into(),
            ago(i["built_at"].as_u64()),
            i["built_by"].as_str().unwrap_or("-").into(),
            i["description"].as_str().unwrap_or("").into(),
        ]);
    }
    table(rows);
    let d = &v["default"];
    let name = d["name"].as_str().unwrap_or_default();
    if d["exists"] != true {
        println!("\nThe default image {name} is not built: isb workspace image build");
    } else if d["current"] != true {
        println!("\n{name} is from an older default recipe: isb workspace image build updates it");
    }
    for b in v["builds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["state"] == "running")
    {
        println!(
            "building {} (isb workspace image logs {})",
            b["name"].as_str().unwrap_or_default(),
            b["id"].as_str().unwrap_or_default()
        );
    }
    Ok(0)
}

pub fn image(cmd: ImageCmd) -> Result<u8> {
    match cmd {
        ImageCmd::Build {
            name,
            recipe,
            base,
            description,
            timeout,
            force,
            no_follow,
        } => {
            let mut a = json!({"force": force});
            opt(&mut a, "name", name);
            opt(
                &mut a,
                "recipe",
                recipe.as_deref().map(read_file).transpose()?,
            );
            opt(&mut a, "base", base);
            opt(&mut a, "description", description);
            opt(&mut a, "timeout", timeout);
            let v = call("workspace_image_build", a, SHORT)?;
            let id = v["id"].as_str().unwrap_or_default().to_string();
            if no_follow {
                println!("{id}");
                return Ok(0);
            }
            eprintln!(
                "building {} from {} (build {id})",
                v["name"].as_str().unwrap_or_default(),
                v["base"].as_str().unwrap_or_default()
            );
            follow(&id)
        }
        ImageCmd::Logs { id } => follow(&id),
        ImageCmd::Ls { json } => list(json),
        ImageCmd::Rm { name } => {
            let v: Value = call("workspace_image_remove", json!({"name": name}), SHORT)?;
            println!(
                "removed {}{}",
                name,
                if v["image_deleted"] == true {
                    ""
                } else {
                    " (the image is kept: another alias names it)"
                }
            );
            Ok(0)
        }
    }
}
