//! Embeds the web UI (`web/dist`, built by `bun run build` in `web/`) into
//! the binary as a table of `include_bytes!`, so `isb serve` needs no files
//! at runtime and the static musl build gains no dependency.
//!
//! The UI is optional at build time: without `web/dist/index.html` the table
//! holds a placeholder page that says the UI was not built, so a plain
//! `cargo build` never needs bun. Release builds set `ISB_WEB_REQUIRED=1`,
//! which turns a missing UI into a build error instead.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>isb</title>
<style>
  :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; }
  main { max-width: 34rem; padding: 2rem; line-height: 1.5; }
  code { font-family: ui-monospace, monospace; font-size: .9em; }
</style>
</head>
<body>
<main>
<h1>isb</h1>
<p>This <code>isb</code> binary was built without its web UI.</p>
<p>The API is up: <code>/api/v1/openapi.json</code> describes it, and
<code>/mcp</code> serves the MCP tools. To include the UI, run
<code>bun install &amp;&amp; bun run build</code> in <code>web/</code>
before <code>cargo build</code>.</p>
</main>
</body>
</html>
"#;

fn main() {
    println!("cargo:rerun-if-env-changed=ISB_WEB_DIST");
    println!("cargo:rerun-if-env-changed=ISB_WEB_REQUIRED");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dist = std::env::var_os("ISB_WEB_DIST")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("web/dist"));
    // A directory is scanned recursively for changes; a missing one is
    // watched for appearing.
    println!("cargo:rerun-if-changed={}", dist.display());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());

    let mut files = Vec::new();
    let built = dist.join("index.html").is_file();
    if built {
        walk(&dist, &dist, &mut files);
        files.sort();
    } else {
        if std::env::var_os("ISB_WEB_REQUIRED").is_some_and(|v| !v.is_empty() && v != "0") {
            panic!(
                "ISB_WEB_REQUIRED is set but {} has no index.html: run `bun install --frozen-lockfile && bun run build` in web/ first",
                dist.display()
            );
        }
        let p = out.join("placeholder.html");
        std::fs::write(&p, PLACEHOLDER).unwrap();
        files.push(("/index.html".to_string(), p));
    }

    let mut src = String::new();
    writeln!(
        src,
        "/// True when the real UI was embedded, false for the placeholder."
    )
    .unwrap();
    writeln!(src, "pub const BUILT: bool = {built};").unwrap();
    writeln!(src, "/// `(url path, bytes)`, sorted by path.").unwrap();
    writeln!(src, "pub static ASSETS: &[(&str, &[u8])] = &[").unwrap();
    for (url, path) in &files {
        let path = path.to_str().expect("web asset paths must be UTF-8");
        writeln!(src, "    ({url:?}, include_bytes!({path:?})),").unwrap();
    }
    writeln!(src, "];").unwrap();
    std::fs::write(out.join("web_assets.rs"), src).unwrap();
}

/// Every file under `dir` as `("/relative/path", absolute path)`, skipping
/// source maps and dotfiles.
fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            walk(root, &p, files);
        } else if ft.is_file() && !name.ends_with(".map") {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            files.push((format!("/{rel}"), p));
        }
    }
}
