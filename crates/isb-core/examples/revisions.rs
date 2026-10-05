//! Print the revision this build computes for every stored stack service,
//! one `<org> <stack> <service> <revision>` line each, from the daemon's
//! state (`revisions ~/.local/state/isb/orgs`).
//!
//! Before upgrading a running daemon, compare this with the `user.isb.rev`
//! labels of the live instances: a service whose revision differs is rolled
//! by the new version as soon as it starts.

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::args().nth(1) else {
        eprintln!("usage: revisions STATE_DIR/orgs");
        std::process::exit(2);
    };
    let mut out = Vec::new();
    for org in std::fs::read_dir(&root)? {
        let stacks = org?.path().join("stacks");
        if !stacks.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&stacks)? {
            let p = f?.path();
            if p.extension().is_some_and(|e| e == "json") {
                print_def(&p, &mut out)?;
            }
        }
    }
    out.sort();
    for l in out {
        println!("{l}");
    }
    Ok(())
}

fn print_def(p: &Path, out: &mut Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let d: isb_core::stack::StackDef = serde_json::from_str(&std::fs::read_to_string(p)?)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    for s in d.file.services.keys() {
        out.push(format!("{} {} {s} {}", d.org, d.name, d.revision(s)?));
    }
    Ok(())
}
