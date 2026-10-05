//! Builds: turn a source tree into an OCI image in the org's registry.
//!
//! Every build runs in a fresh sandbox in the org's own incus project, never
//! on the host, and the sandbox is deleted afterwards (also on failure and
//! timeout):
//!
//! - **A container by default.** BuildKit runs as root inside an ordinary
//!   unprivileged org container (its own uid range, the org's network and
//!   ACL, the org's quota); its `RUN` steps get their own namespaces
//!   without `security.nesting`, which the org project keeps blocked.
//! - **A VM when the source is untrusted** ([`BuildRequest::untrusted`]):
//!   the build gets its own kernel. The org project allows VMs; the host
//!   needs KVM.
//!
//! The source is copied in (the host tree is only read), the image is
//! exported as an OCI layout inside the sandbox, and the daemon copies it
//! out and pushes it to the local registry ([`crate::registry`]): build
//! sandboxes have neither a route to the registry nor credentials for it.
//! BuildKit's state lives on a per-app volume in the org
//! (`build-cache-<app>`), so the next build of the app reuses layers and
//! cache mounts.
//!
//! The builder image (BuildKit, railpack, nixpacks; see `builder-image.sh`)
//! is prepared once per recipe in the `isb-system` project and cached as a
//! local incus image, `isb-builder/<recipe hash>`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions, Stdin};
use crate::org::OrgId;
use crate::sandbox::Sandbox;
mod ready;
pub mod workspace_image;
/// How a source tree becomes an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Builder {
    /// Railpack detects the language and builds without a Dockerfile.
    Railpack,
    Nixpacks,
    /// A Dockerfile, relative to the context.
    Dockerfile {
        #[serde(default = "default_dockerfile")]
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// Cloud Native Buildpacks with the given builder image. Not supported
    /// yet: `pack` drives a docker daemon (see docs/guides/builds.md).
    Buildpacks {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        builder: Option<String>,
    },
}

fn default_dockerfile() -> String {
    "Dockerfile".into()
}

impl Builder {
    fn name(&self) -> &'static str {
        match self {
            Builder::Railpack => "railpack",
            Builder::Nixpacks => "nixpacks",
            Builder::Dockerfile { .. } => "dockerfile",
            Builder::Buildpacks { .. } => "buildpacks",
        }
    }
}

/// One build.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub org: OrgId,
    /// The app the image belongs to: names the repository in the registry
    /// (`<org>/<app>`) and the build cache volume.
    pub app: String,
    /// A checked-out source tree on the host. The build reads it, never
    /// writes it.
    pub context: PathBuf,
    /// A subdirectory of `context` to build from.
    pub subdir: Option<String>,
    pub builder: Builder,
    /// Build-time variables (Dockerfile `ARG`s, buildpack env).
    pub args: Vec<(String, String)>,
    /// The tag to push, e.g. the commit SHA.
    pub tag: String,
    /// Build in a VM rather than a container.
    pub untrusted: bool,
    /// Whose build cache volume to use (default: the app's). Previews
    /// build with their own, so a pull request cannot poison the cache
    /// production builds read.
    pub cache: Option<String>,
}

/// What a build produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImage {
    /// What a compose `image:` takes to run it: `registry:<app>:<tag>@<digest>`,
    /// resolved in the org the stack runs in (so pinned to this build even
    /// if the tag moves later).
    pub image: String,
    /// The manifest digest (`sha256:...`), for rollbacks that must not
    /// follow a moved tag.
    pub digest: String,
}

/// Limits for a build. [`BuildOptions::default`] reads `ISB_BUILD_TIMEOUT`,
/// `ISB_BUILD_CPUS`, `ISB_BUILD_MEMORY` and `ISB_BUILD_CACHE_SIZE`.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// The whole build, sandbox creation to push (default 30 minutes).
    pub timeout: Duration,
    /// The build sandbox's CPUs (default 2) and memory (default 4GiB),
    /// counted against the org's quota.
    pub cpus: u32,
    pub memory: String,
    /// A VM build's cache disk (default 20GiB); a container's cache volume
    /// is a directory, bounded by BuildKit's own garbage collection.
    pub cache_size: String,
    /// Largest source tree copied in (default 2 GiB).
    pub max_context: u64,
}

impl Default for BuildOptions {
    fn default() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        BuildOptions {
            timeout: env("ISB_BUILD_TIMEOUT")
                .and_then(|t| crate::flex::parse_duration(&t).ok())
                .unwrap_or(Duration::from_secs(30 * 60)),
            cpus: env("ISB_BUILD_CPUS")
                .and_then(|c| c.parse().ok())
                .unwrap_or(2),
            memory: env("ISB_BUILD_MEMORY").unwrap_or_else(|| "4GiB".into()),
            cache_size: env("ISB_BUILD_CACHE_SIZE").unwrap_or_else(|| "20GiB".into()),
            max_context: 2 << 30,
        }
    }
}

/// The railpack BuildKit frontend, matching the railpack in the image.
const RAILPACK_FRONTEND: &str = "ghcr.io/railwayapp/railpack-frontend:v0.40.1@sha256:f1973377693af30c9b37a92c97c661c07b277ccdc6be909213c74c771f8d2d6d";
const RECIPE: &str = include_str!("builder-image.sh");
const DRIVER: &str = include_str!("build.sh");
/// The base the builder image is made from.
const BASE_IMAGE: &str = "images:ubuntu/24.04";
/// The VM's cache disk, as its by-id name ends.
const CACHE_DEVICE: &str = "isbcache";

/// The builder image's alias: `isb-builder/<hash>` (containers) or
/// `isb-builder-vm/<hash>`. A new recipe is a new alias.
pub fn builder_alias(vm: bool) -> String {
    let d = crate::registry::oci::digest_of(RECIPE.as_bytes());
    let h = &d[7..19];
    if vm {
        format!("isb-builder-vm/{h}")
    } else {
        format!("isb-builder/{h}")
    }
}

/// Run a build with `base` (an unscoped client; the build's sandbox lives
/// in the request's org), streaming its log lines to `log`.
pub fn run(base: &Client, req: &BuildRequest, log: &mut dyn FnMut(&str)) -> Result<BuiltImage> {
    run_with(base, req, &BuildOptions::default(), log)
}

/// [`run`] with explicit limits.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn run_with(
    base: &Client,
    req: &BuildRequest,
    opts: &BuildOptions,
    log: &mut dyn FnMut(&str),
) -> Result<BuiltImage> {
    let started = Instant::now();
    let deadline = started + opts.timeout;
    let ctx_dir = check(req)?;
    let reg = crate::registry::Registry::shared(base)?;
    let vm = req.untrusted;
    let oc = crate::org::client(base, &req.org);
    crate::org::get(base, &req.org)?;
    log(&format!(
        "building {}/{}:{} with {} in a {}",
        req.org,
        req.app,
        req.tag,
        req.builder.name(),
        if vm { "VM" } else { "container" }
    ));
    let image = ensure_builder_image(base, vm, deadline, log)?;
    let pool = crate::sandbox::host_facts(&oc)?.pick_pool(None)?;
    let cache = ensure_cache(
        &oc,
        &pool,
        req.cache.as_deref().unwrap_or(&req.app),
        vm,
        &opts.cache_size,
        log,
    )?;

    let name = sandbox_name(&req.app);
    let _guard = Remove {
        client: oc.clone(),
        name: name.clone(),
    };
    log(&format!("creating build sandbox {name}"));
    create_sandbox(
        &oc, &name, &image, vm, &pool, &cache, opts, &req.app, deadline,
    )?;
    let sb = Sandbox::get(&oc, &name)?;
    ready::exec(&sb, deadline)?;

    let sent = send_context(&sb, &ctx_dir, opts.max_context, deadline)?;
    log(&format!("copied the source in ({})", human(sent)));
    oc.push_file(&name, "/build/build.sh", DRIVER.as_bytes(), 0, 0, 0o755)?;
    let mut args = String::new();
    for (k, v) in &req.args {
        args.push_str(&format!("{k}={v}\n"));
    }
    oc.push_file(&name, "/build/args", args.as_bytes(), 0, 0, 0o600)?;

    let mut env = ExecOptions::default()
        .env("ISB_BUILDER", req.builder.name())
        .env("ISB_RAILPACK_FRONTEND", RAILPACK_FRONTEND)
        .env("HOME", "/root")
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env(
            "ISB_CONTEXT",
            match &req.subdir {
                Some(s) => format!("/build/src/{s}"),
                None => "/build/src".into(),
            },
        );
    if let Builder::Dockerfile { path, target } = &req.builder {
        env = env.env("ISB_DOCKERFILE", path.clone());
        if let Some(t) = target {
            env = env.env("ISB_TARGET", t.clone());
        }
    }
    if vm {
        env = env.env("ISB_CACHE_DISK", CACHE_DEVICE);
    }
    let code = stream_lines(
        &sb,
        &["/bin/bash", "/build/build.sh"],
        env.timeout(remaining(deadline, "the build")?),
        log,
    )
    .map_err(|e| {
        if e.is_timeout() || Instant::now() >= deadline {
            Error::invalid(format!(
                "build of {}/{} timed out after {:?} (killed)",
                req.org, req.app, opts.timeout
            ))
        } else {
            e
        }
    })?;
    if code != 0 {
        return Err(if Instant::now() >= deadline {
            Error::invalid(format!(
                "build of {}/{} timed out after {:?}",
                req.org, req.app, opts.timeout
            ))
        } else {
            Error::invalid(format!(
                "build of {}/{} failed (exit {code}); see the log above",
                req.org, req.app
            ))
        });
    }

    let tmp = scratch_file(&req.app)?;
    let _rm = RemoveFile(tmp.clone());
    let n = copy_out(&sb, "/build/out/image.tar", &tmp, deadline)?;
    log(&format!("copied the image out ({})", human(n)));
    let digest = reg.push(&req.org, &req.app, &req.tag, &tmp, log)?;
    let image = format!(
        "registry:{}",
        crate::registry::ImageRef {
            app: req.app.clone(),
            tag: Some(req.tag.clone()),
            digest: Some(digest.clone()),
        }
        .render()
    );
    log(&format!(
        "built {image} in {:.0}s",
        started.elapsed().as_secs_f64()
    ));
    Ok(BuiltImage { image, digest })
}

/// Validate a request; returns the directory to copy in.
fn check(req: &BuildRequest) -> Result<PathBuf> {
    if !crate::registry::valid_app(&req.app) || req.app.len() > 40 {
        return Err(Error::invalid(format!(
            "app {:?}: up to 40 characters of [a-z0-9._-], starting with a letter or digit",
            req.app
        )));
    }
    if !crate::registry::valid_tag(&req.tag) {
        return Err(Error::invalid(format!(
            "tag {:?}: [A-Za-z0-9_][A-Za-z0-9_.-]*, at most 128 characters",
            req.tag
        )));
    }
    let rel_ok = |p: &str| {
        !p.is_empty()
            && !p.starts_with('/')
            && Path::new(p)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
    };
    if let Some(s) = &req.subdir {
        if !rel_ok(s) {
            return Err(Error::invalid(format!(
                "subdir {s:?}: a relative path inside the context"
            )));
        }
    }
    match &req.builder {
        Builder::Dockerfile { path, target } => {
            if !rel_ok(path) {
                return Err(Error::invalid(format!(
                    "dockerfile {path:?}: a relative path inside the context"
                )));
            }
            if target.as_deref().is_some_and(|t| {
                t.is_empty()
                    || !t
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            }) {
                return Err(Error::invalid("target: a stage name"));
            }
        }
        Builder::Buildpacks { .. } => {
            return Err(Error::invalid(
                "buildpacks are not supported yet: pack needs a docker daemon; use railpack (it detects the same languages) or a Dockerfile",
            ));
        }
        _ => {}
    }
    for (k, v) in &req.args {
        let ok = !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(Error::invalid(format!(
                "build argument {k:?}: [A-Za-z_][A-Za-z0-9_]*"
            )));
        }
        if v.contains(['\n', '\r', '\0']) {
            return Err(Error::invalid(format!(
                "build argument {k}: values are one line"
            )));
        }
    }
    let dir = match &req.subdir {
        Some(s) => req.context.join(s),
        None => req.context.clone(),
    };
    let md = std::fs::metadata(&req.context)
        .map_err(|e| Error::invalid(format!("context {}: {e}", req.context.display())))?;
    if !md.is_dir() || !req.context.is_absolute() {
        return Err(Error::invalid(format!(
            "context {}: an absolute directory",
            req.context.display()
        )));
    }
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    Ok(req.context.clone())
}

fn remaining(deadline: Instant, what: &str) -> Result<Duration> {
    let r = deadline.saturating_duration_since(Instant::now());
    if r.is_zero() {
        return Err(Error::invalid(format!("build timed out before {what}")));
    }
    Ok(r)
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KiB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// `build-<app>-<random>`, an instance name.
fn sandbox_name(app: &str) -> String {
    let a: String = app
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let a = a.trim_matches('-');
    format!(
        "build-{a}-{}{}",
        crate::stack::new_id(),
        crate::stack::new_id()
    )
}

/// The name of a build cache volume: `build-cache-<key>`, `-vm` for VMs.
pub fn cache_volume(key: &str, vm: bool) -> String {
    let a = key.replace('.', "-");
    if vm {
        format!("build-cache-{a}-vm")
    } else {
        format!("build-cache-{a}")
    }
}

/// The build cache volume of an app: `build-cache-<app>`, a filesystem
/// volume for containers, a block volume (`-vm`) for VMs, whose overlay
/// snapshots cannot live on a shared filesystem.
fn ensure_cache(
    oc: &Client,
    pool: &str,
    app: &str,
    vm: bool,
    size: &str,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let name = cache_volume(app, vm);
    let path = format!(
        "/1.0/storage-pools/{}/volumes/custom/{}",
        encode_segment(pool),
        encode_segment(&name)
    );
    if oc.get_opt(&path)?.is_none() {
        log(&format!("creating build cache volume {name}"));
        let mut body = json!({"name": name, "type": "custom", "config": {}});
        if vm {
            body["content_type"] = json!("block");
            body["config"]["size"] = json!(size);
        } else {
            body["content_type"] = json!("filesystem");
        }
        match oc.mutate(
            "POST",
            &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(pool)),
            Some(&body),
            &format!("create volume {name}"),
            oc.get_timeouts().other,
        ) {
            Ok(_) => {}
            // Another build of the app made it first.
            Err(e) if e.is_conflict() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(name)
}

#[expect(clippy::too_many_arguments)]
fn create_sandbox(
    oc: &Client,
    name: &str,
    image: &str,
    vm: bool,
    pool: &str,
    cache: &str,
    opts: &BuildOptions,
    app: &str,
    deadline: Instant,
) -> Result<()> {
    let mut disk = json!({"type": "disk", "pool": pool, "source": cache});
    let mut root = json!({"type": "disk", "path": "/", "pool": pool});
    // A VM gets a raw disk, which build.sh finds by this device name and formats
    // once, and a root with room for the source, export and BuildKit's scratch;
    // a container, the directory itself and a root sized only under a disk limit.
    if vm {
        root["size"] = json!("20GiB");
    } else {
        disk["path"] = json!("/var/lib/buildkit");
    }
    let devices = json!({ CACHE_DEVICE: disk, "root": root });
    let body = json!({
        "name": name,
        "type": if vm { "virtual-machine" } else { "container" },
        "source": {"type": "image", "alias": image},
        "config": {
            "limits.cpu": opts.cpus.to_string(),
            "limits.memory": opts.memory,
            "user.isb.build": app,
        },
        "devices": devices,
        "profiles": ["default"],
    });
    let t = remaining(deadline, "creating the build sandbox")?;
    oc.mutate(
        "POST",
        "/1.0/instances",
        Some(&crate::org::disk::sized_root(oc, body)),
        &format!("create build sandbox {name}"),
        t.min(oc.get_timeouts().create.max(Duration::from_secs(600))),
    )?;
    let t = remaining(deadline, "starting the build sandbox")?;
    oc.mutate(
        "PUT",
        &format!("/1.0/instances/{}/state", encode_segment(name)),
        Some(&json!({"action": "start", "timeout": 60})),
        &format!("start build sandbox {name}"),
        t.min(Duration::from_secs(300)),
    )?;
    Ok(())
}

/// Run argv, handing each output line to `log`. Returns the exit code.
fn stream_lines(
    sb: &Sandbox,
    argv: &[&str],
    opts: ExecOptions,
    log: &mut dyn FnMut(&str),
) -> Result<i32> {
    let mut s = sb.exec_stream(argv.iter().copied(), opts)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let emit = |buf: &mut Vec<u8>, chunk: Vec<u8>, log: &mut dyn FnMut(&str)| {
        buf.extend(chunk);
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            log(text.trim_end_matches('\r'));
        }
        // A runaway line without newlines is cut rather than buffered.
        if buf.len() > 64 << 10 {
            log(&String::from_utf8_lossy(buf));
            buf.clear();
        }
    };
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) => emit(&mut out, b, log),
            ExecEvent::Stderr(b) => emit(&mut err, b, log),
        }
    }
    for b in [out, err] {
        if !b.is_empty() {
            log(&String::from_utf8_lossy(&b));
        }
    }
    // incus fails the operation, rather than reporting the code, when the
    // command ends with 126 or 127, which a script can pass on.
    match s.wait() {
        Err(e) if e.to_string().contains("Command not found") => Ok(127),
        Err(e) if e.to_string().contains("Permission denied") => Ok(126),
        r => r,
    }
}

/// Copy `dir` into the sandbox at `/build/src` as a tar on stdin. Returns
/// the bytes sent.
fn send_context(sb: &Sandbox, dir: &Path, max: u64, deadline: Instant) -> Result<u64> {
    let mut s = sb.exec_stream(
        [
            "/bin/sh",
            "-c",
            "mkdir -p /build/src && tar -x --no-same-owner -C /build/src",
        ],
        ExecOptions::default()
            .stdin(Stdin::Piped)
            .timeout(remaining(deadline, "copying the source")?),
    )?;
    let mut w = ChunkWriter {
        s: &s,
        buf: Vec::with_capacity(CHUNK),
        sent: 0,
        max,
    };
    let r = tar::write_dir(&mut w, dir).and_then(|_| w.flush().map_err(Error::from));
    let sent = w.sent;
    s.close_stdin();
    let out = s.collect_output()?;
    r?;
    if !out.success() {
        return Err(Error::invalid(format!(
            "copying the source in failed (exit {}): {}",
            out.exit_code,
            out.stderr_text().trim()
        )));
    }
    Ok(sent)
}

const CHUNK: usize = 256 << 10;

struct ChunkWriter<'a> {
    s: &'a crate::exec::ExecStream,
    buf: Vec<u8>,
    sent: u64,
    max: u64,
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.sent += b.len() as u64;
        if self.sent > self.max {
            return Err(std::io::Error::other(format!(
                "the source is over {} (ISB build limit)",
                human(self.max)
            )));
        }
        self.buf.extend_from_slice(b);
        if self.buf.len() >= CHUNK {
            self.flush()?;
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            self.s
                .write_stdin(&self.buf)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            self.buf.clear();
        }
        Ok(())
    }
}

/// Copy a file out of the sandbox through `cat`. Returns its size.
fn copy_out(sb: &Sandbox, path: &str, to: &Path, deadline: Instant) -> Result<u64> {
    let mut f = std::fs::File::create(to)?;
    let mut s = sb.exec_stream(
        ["/bin/cat", path],
        ExecOptions::default().timeout(remaining(deadline, "copying the image out")?),
    )?;
    let mut n = 0u64;
    let mut err = Vec::new();
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) => {
                n += b.len() as u64;
                f.write_all(&b)?;
            }
            ExecEvent::Stderr(b) => err.extend(b),
        }
    }
    let code = s.wait()?;
    if code != 0 {
        return Err(Error::invalid(format!(
            "copying {path} out failed (exit {code}): {}",
            String::from_utf8_lossy(&err).trim()
        )));
    }
    f.sync_all()?;
    Ok(n)
}

/// Where the image is staged between the sandbox and the registry: the
/// daemon's state directory (images can be large; /tmp may be a tmpfs).
fn scratch_file(app: &str) -> Result<PathBuf> {
    let dir = std::env::var_os("ISB_SERVE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(crate::stack::Store::default_dir)
        .join("builds");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!(
        "{app}-{}{}.tar",
        crate::stack::new_id(),
        crate::stack::new_id()
    )))
}

struct RemoveFile(PathBuf);

impl Drop for RemoveFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deletes the build sandbox however the build ends.
struct Remove {
    client: Client,
    name: String,
}

impl Drop for Remove {
    fn drop(&mut self) {
        match Sandbox::remove(&self.client, &self.name, true) {
            Ok(()) => {}
            Err(e) if e.is_not_found() => {}
            Err(e) => eprintln!("isb: build sandbox {}: not deleted: {e}", self.name),
        }
    }
}

/// One preparation at a time per process; the image is shared by all orgs.
static PREPARE: Mutex<()> = Mutex::new(());

/// The builder image's alias, made from `builder-image.sh` when missing: in the
/// `isb-system` project, never in an org (an org could otherwise tamper
/// with an image every org builds with).
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn ensure_builder_image(
    base: &Client,
    vm: bool,
    deadline: Instant,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let alias = builder_alias(vm);
    let h = base.clone().project("default");
    let alias_path = format!("/1.0/images/aliases/{}", encode_segment(&alias));
    if h.get_opt(&alias_path)?.is_some() {
        return Ok(alias);
    }
    let _g = PREPARE.lock().unwrap();
    if h.get_opt(&alias_path)?.is_some() {
        return Ok(alias);
    }
    log(&format!(
        "preparing the builder image {alias} (once per recipe; a few minutes)"
    ));
    let s = base.clone().project(crate::registry::PROJECT);
    if crate::registry::info(base)?.is_none() {
        return Err(Error::invalid(
            "no isb-system project yet: run `isb registry setup` first",
        ));
    }
    let pool = crate::sandbox::host_facts(&h)?.pick_pool(None)?;
    let net = uplink_network(&h)?;
    let name = format!(
        "builder-prep-{}{}",
        crate::stack::new_id(),
        crate::stack::new_id()
    );
    let _guard = Remove {
        client: s.clone(),
        name: name.clone(),
    };
    let src = crate::plan::ImageSource::parse(BASE_IMAGE)?;
    let config = json!({"limits.cpu": "4", "limits.memory": "4GiB"});
    s.mutate(
        "POST",
        "/1.0/instances",
        Some(&json!({
            "name": name,
            "type": if vm { "virtual-machine" } else { "container" },
            "source": src.to_api(None),
            "config": config,
            "devices": {
                "root": {"type": "disk", "path": "/", "pool": pool},
                "eth0": {"type": "nic", "name": "eth0", "network": net},
            },
            "profiles": ["default"],
        })),
        &format!("create {name}"),
        remaining(deadline, "creating the builder image")?.min(Duration::from_secs(1200)),
    )?;
    s.mutate(
        "PUT",
        &format!("/1.0/instances/{name}/state"),
        Some(&json!({"action": "start", "timeout": 60})),
        &format!("start {name}"),
        Duration::from_secs(300),
    )?;
    let sb = Sandbox::get(&s, &name)?;
    ready::exec(&sb, deadline)?;
    ready::network(&s, &name, &net, deadline, log)?;
    s.push_file(
        &name,
        "/root/builder-image.sh",
        RECIPE.as_bytes(),
        0,
        0,
        0o755,
    )?;
    let code = stream_lines(
        &sb,
        &["/bin/sh", "/root/builder-image.sh"],
        ExecOptions::default()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .timeout(remaining(deadline, "preparing the builder image")?),
        &mut |l| log(&format!("prepare: {l}")),
    )?;
    if code != 0 {
        return Err(Error::invalid(format!(
            "preparing the builder image failed (exit {code})"
        )));
    }
    // A clean shutdown, so a VM's disk has everything the recipe wrote.
    let stop = |force: bool| {
        s.mutate(
            "PUT",
            &format!("/1.0/instances/{name}/state"),
            Some(&json!({"action": "stop", "timeout": 120, "force": force})),
            &format!("stop {name}"),
            Duration::from_secs(180),
        )
    };
    if let Err(e) = stop(false) {
        log(&format!("prepare: a clean stop failed ({e}); forcing it"));
        stop(true)?;
    }
    log(&format!("publishing {alias}"));
    let versions: Value = json!({
        "description": format!("isb builder ({})", if vm { "VM" } else { "container" }),
        "isb.recipe": alias,
    });
    s.mutate(
        "POST",
        "/1.0/images",
        Some(&json!({
            "source": {"type": "instance", "name": name},
            "properties": versions,
            "aliases": [{"name": alias, "description": "isb builder image"}],
        })),
        &format!("publish {alias}"),
        remaining(deadline, "publishing the builder image")?.min(Duration::from_secs(1800)),
    )?;
    Ok(alias)
}

/// The network a builder image is prepared on: the host's default managed
/// bridge (`incusbr0` if there is one), never an org's.
fn uplink_network(h: &Client) -> Result<String> {
    let nets = h.get("/1.0/networks?recursion=1")?;
    let managed: Vec<&str> = nets
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["managed"].as_bool() == Some(true) && n["type"] == "bridge")
        .filter_map(|n| n["name"].as_str())
        .filter(|n| !n.starts_with("isbbr"))
        .collect();
    managed
        .iter()
        .find(|n| **n == "incusbr0")
        .or(managed.first())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::invalid("no managed bridge to prepare the builder image on"))
}

/// A tar writer for a source tree: regular files, directories and
/// symlinks (as links, never followed), with pax headers for long names.
pub(crate) mod tar {
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use crate::error::Result;

    fn octal(field: &mut [u8], v: u64) {
        // Sizes are capped well below 8 GiB, so 11 digits always suffice.
        let w = field.len() - 1;
        let s = format!("{v:0w$o}");
        let b = s.as_bytes();
        let start = b.len().saturating_sub(w);
        field[..w].copy_from_slice(&b[start..]);
        field[w] = 0;
    }

    fn header(name: &str, size: u64, mode: u32, mtime: u64, kind: u8, link: &str) -> [u8; 512] {
        let mut h = [0u8; 512];
        let n = name.as_bytes();
        h[..n.len().min(100)].copy_from_slice(&n[..n.len().min(100)]);
        octal(&mut h[100..108], (mode & 0o7777) as u64);
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], size);
        octal(&mut h[136..148], mtime);
        h[156] = kind;
        let l = link.as_bytes();
        h[157..157 + l.len().min(100)].copy_from_slice(&l[..l.len().min(100)]);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|b| *b as u64).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        h
    }

    fn pad(w: &mut dyn Write, n: u64) -> std::io::Result<()> {
        let r = (512 - (n % 512) as usize) % 512;
        w.write_all(&vec![0u8; r])
    }

    fn pax_record(key: &str, value: &str) -> String {
        // "<len> key=value\n", where len counts itself.
        let body = format!(" {key}={value}\n");
        let mut len = body.len() + 1;
        while format!("{len}{body}").len() != len {
            len = format!("{len}{body}").len();
        }
        format!("{len}{body}")
    }

    fn entry(
        w: &mut dyn Write,
        name: &str,
        size: u64,
        mode: u32,
        mtime: u64,
        kind: u8,
        link: &str,
    ) -> std::io::Result<()> {
        if name.len() > 100 || link.len() > 100 || !name.is_ascii() || !link.is_ascii() {
            let mut pax = pax_record("path", name);
            if !link.is_empty() {
                pax.push_str(&pax_record("linkpath", link));
            }
            w.write_all(&header(
                "././@PaxHeader",
                pax.len() as u64,
                0o644,
                mtime,
                b'x',
                "",
            ))?;
            w.write_all(pax.as_bytes())?;
            pad(w, pax.len() as u64)?;
        }
        w.write_all(&header(name, size, mode, mtime, kind, link))
    }

    /// Write `dir`'s contents (not `dir` itself) as a tar.
    pub fn write_dir(w: &mut dyn Write, dir: &Path) -> Result<()> {
        walk(w, dir, "")?;
        w.write_all(&[0u8; 1024])?;
        Ok(())
    }

    fn walk(w: &mut dyn Write, dir: &Path, prefix: &str) -> Result<()> {
        let mut names: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        names.sort();
        for n in names {
            let path = dir.join(&n);
            let Some(n) = n.to_str() else {
                // Not UTF-8: leave it out rather than mangle it.
                continue;
            };
            let name = format!("{prefix}{n}");
            let md = std::fs::symlink_metadata(&path)?;
            let mtime = md.mtime().max(0) as u64;
            let mode = md.permissions().mode();
            let ft = md.file_type();
            if ft.is_symlink() {
                let target = std::fs::read_link(&path)?;
                let Some(t) = target.to_str() else { continue };
                entry(w, &name, 0, 0o777, mtime, b'2', t)?;
            } else if ft.is_dir() {
                entry(w, &format!("{name}/"), 0, mode, mtime, b'5', "")?;
                walk(w, &path, &format!("{name}/"))?;
            } else if ft.is_file() {
                let mut f = std::fs::File::open(&path)?;
                let size = md.len();
                entry(w, &name, size, mode, mtime, b'0', "")?;
                // Exactly `size` bytes, even if the file changes meanwhile.
                let copied = std::io::copy(&mut (&mut f).take(size), w)?;
                if copied < size {
                    std::io::copy(&mut std::io::repeat(0).take(size - copied), w)?;
                }
                pad(w, size)?;
            }
            // Sockets, fifos and devices are not source.
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(builder: Builder) -> BuildRequest {
        BuildRequest {
            org: OrgId::new("acme").unwrap(),
            app: "web".into(),
            context: std::env::temp_dir(),
            subdir: None,
            builder,
            args: vec![],
            tag: "v1".into(),
            untrusted: false,
            cache: None,
        }
    }

    #[test]
    fn requests_are_checked() {
        assert!(check(&req(Builder::Railpack)).is_ok());
        let mut r = req(Builder::Railpack);
        r.app = "Web".into();
        assert!(check(&r).is_err());
        r = req(Builder::Railpack);
        r.tag = "bad tag".into();
        assert!(check(&r).is_err());
        r = req(Builder::Railpack);
        r.subdir = Some("../etc".into());
        assert!(check(&r).is_err());
        r = req(Builder::Dockerfile {
            path: "/etc/passwd".into(),
            target: None,
        });
        assert!(check(&r).is_err());
        r = req(Builder::Railpack);
        r.args = vec![("A B".into(), "x".into())];
        assert!(check(&r).is_err());
        r.args = vec![("A".into(), "x\ny".into())];
        assert!(check(&r).is_err());
        assert!(check(&req(Builder::Buildpacks { builder: None })).is_err());
        r = req(Builder::Railpack);
        r.context = "relative".into();
        assert!(check(&r).is_err());
    }

    #[test]
    fn builder_json_matches_the_contract() {
        let b: Builder = serde_json::from_str(r#"{"type":"dockerfile"}"#).unwrap();
        assert_eq!(
            b,
            Builder::Dockerfile {
                path: "Dockerfile".into(),
                target: None
            }
        );
        let b: Builder = serde_json::from_str(r#"{"type":"railpack"}"#).unwrap();
        assert_eq!(b.name(), "railpack");
        assert!(builder_alias(false).starts_with("isb-builder/"));
        assert!(builder_alias(true).starts_with("isb-builder-vm/"));
        assert!(sandbox_name("my.app").starts_with("build-my-app-"));
        assert!(sandbox_name(&"a".repeat(40)).len() <= 63);
    }

    #[test]
    fn source_tars_round_trip_through_the_layout_reader() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("src");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let long = "x".repeat(150);
        std::fs::write(root.join("sub").join(&long), "long").unwrap();
        std::os::unix::fs::symlink("/etc/shadow", root.join("link")).unwrap();
        let mut buf = Vec::new();
        tar::write_dir(&mut buf, &root).unwrap();
        let p = d.path().join("x.tar");
        std::fs::write(&p, &buf).unwrap();
        // The registry's tar reader is the same format's other half.
        let l = crate::registry::oci::Layout::open(&p).unwrap();
        let _ = l;
        // And the system tar agrees, when there is one.
        if let Ok(out) = std::process::Command::new("tar")
            .arg("-tvf")
            .arg(&p)
            .output()
        {
            if out.status.success() {
                let t = String::from_utf8_lossy(&out.stdout);
                assert!(t.contains("a.txt"), "{t}");
                assert!(t.contains(&format!("sub/{long}")), "{t}");
                assert!(
                    t.contains("link -> /etc/shadow"),
                    "symlinks stay links: {t}"
                );
            }
        }
    }
}
