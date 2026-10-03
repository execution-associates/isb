//! `isb machine`: incus runs only on Linux, so on macOS isb manages a Lima VM
//! that runs it, the way `podman machine` does.
//!
//! The VM is plain Lima (`limactl`, installed separately) driven by a YAML
//! file isb generates into `~/.isb/machine/<name>/lima.yaml`:
//!
//! - Ubuntu 24.04 with incus from Zabbly's stable channel. incus's socket is
//!   owned by the Lima user (a `SocketUser` drop-in), because Lima forwards it
//!   over an ssh connection that was opened before any group change.
//! - `$HOME` mounted writable at the same path (virtiofs), so a bind source
//!   like `./app` resolves to the same absolute path on the Mac and in the VM.
//!   The guest user has the Mac user's uid, and files keep the Mac's uid/gid,
//!   so `idmap: auto` maps those ids (not 1000) onto guest 1000; root is
//!   delegated both in the VM's `/etc/subuid` and `/etc/subgid`.
//! - The incus socket and `isb serve`'s socket forwarded to
//!   `~/.isb/machine/<name>/{incus,serve}.sock`, and loopback listeners (incus
//!   proxy devices, the stack balancer, the daemon's 8092) forwarded to the
//!   Mac's 127.0.0.1 by Lima.
//! - `isb serve` runs in the VM as a system unit, as the Linux binary, next to
//!   the bridges its balancer must reach.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::client::{Client, Timeouts};
use crate::error::{Error, Result};

/// The machine every command uses unless told otherwise, and the one whose
/// sockets [`Client::default_socket`] and the serve socket fall back to.
pub const DEFAULT_NAME: &str = "isb";
/// The daemon's HTTP listener in the VM, forwarded to the same address on the Mac.
pub const SERVE_LISTEN: &str = "127.0.0.1:8092";
/// launchd label of the agent `isb serve install` writes on macOS.
pub const LAUNCH_AGENT_LABEL: &str = "dev.isb.machine";

const GUEST_INCUS_SOCKET: &str = "/var/lib/incus/unix.socket";
const GUEST_SERVE_SOCKET: &str = "/run/isb/serve.sock";
/// Written by the provisioning script once incus is installed and initialised.
const PROVISIONED_MARKER: &str = "/var/lib/isb-machine/provisioned";
const RELEASES: &str = "https://github.com/execution-associates/isb/releases/download";

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::invalid("HOME is not set"))
}

/// A machine name doubles as the Lima instance name.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "machine name {name:?}: use 1-32 lowercase letters, digits and dashes"
        )))
    }
}

/// `~/.isb/machine/<name>`: the Lima YAML, the staged guest binary and the
/// forwarded sockets.
pub fn dir(name: &str) -> Result<PathBuf> {
    Ok(home()?.join(".isb/machine").join(name))
}

pub fn incus_socket(name: &str) -> Result<PathBuf> {
    Ok(dir(name)?.join("incus.sock"))
}

pub fn serve_socket(name: &str) -> Result<PathBuf> {
    Ok(dir(name)?.join("serve.sock"))
}

/// What the generated Lima YAML depends on.
#[derive(Debug, Clone)]
pub struct LimaConfig {
    pub name: String,
    pub cpus: u32,
    pub memory: String,
    pub disk: String,
    /// The Mac user's home, mounted at the same path.
    pub home: PathBuf,
    /// The guest user: the Mac user's name when Linux accepts it.
    pub user: String,
    /// The Mac user's uid and primary gid, which own files in the shared home.
    pub uid: u32,
    pub gid: u32,
}

/// A JSON string is a valid YAML double-quoted scalar.
fn q(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// The guest user name: the Mac user's when it is a valid Linux name, else
/// `lima`, as Lima itself would choose.
pub fn guest_user(mac_user: &str) -> String {
    let ok = !mac_user.is_empty()
        && mac_user.len() <= 32
        && mac_user.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && mac_user
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok {
        mac_user.to_string()
    } else {
        "lima".to_string()
    }
}

/// `4GiB`, `512MiB`, `10G`: digits, an optional fraction, an optional unit.
fn check_size(what: &str, v: &str) -> Result<()> {
    let digits = v.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit = &v[digits.len()..];
    let ok = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.chars().next().is_some_and(|c| c.is_ascii_digit())
        && matches!(
            unit,
            "" | "K" | "M" | "G" | "T" | "KiB" | "MiB" | "GiB" | "TiB" | "KB" | "MB" | "GB" | "TB"
        );
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{what} {v:?}: expected a size like 4GiB or 512MiB"
        )))
    }
}

/// The provisioning script. Lima runs it as root on every boot, so every step
/// is idempotent.
fn provision_script(c: &LimaConfig) -> String {
    format!(
        r#"#!/bin/bash
set -eux -o pipefail
# Files in the shared home keep the Mac's uid {uid} and gid {gid}: let root
# map them into containers (raw.idmap), beside the usual container range.
grep -qx 'root:1000000:1000000000' /etc/subuid || echo 'root:1000000:1000000000' >> /etc/subuid
grep -qx 'root:1000000:1000000000' /etc/subgid || echo 'root:1000000:1000000000' >> /etc/subgid
grep -qx 'root:{uid}:1' /etc/subuid || echo 'root:{uid}:1' >> /etc/subuid
grep -qx 'root:{gid}:1' /etc/subgid || echo 'root:{gid}:1' >> /etc/subgid
# Lima forwards the socket over an ssh connection opened before the user
# joins incus-admin, so the user owns the socket instead.
mkdir -p /etc/systemd/system/incus.socket.d
printf '[Socket]\nSocketUser={user}\n' > /etc/systemd/system/incus.socket.d/isb-machine.conf
systemctl daemon-reload
if ! command -v incus >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://pkgs.zabbly.com/key.asc -o /etc/apt/keyrings/zabbly.asc
  cat > /etc/apt/sources.list.d/zabbly-incus-stable.sources <<EOF
Enabled: yes
Types: deb
URIs: https://pkgs.zabbly.com/incus/stable
Suites: $(. /etc/os-release && echo "$VERSION_CODENAME")
Components: main
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/zabbly.asc
EOF
  apt-get update
  apt-get install -y --no-install-recommends incus
fi
usermod -aG incus-admin {user}
if [ ! -e {marker} ]; then
  incus admin init --auto
  mkdir -p "$(dirname {marker})"
  touch {marker}
fi
"#,
        uid = c.uid,
        gid = c.gid,
        user = c.user,
        marker = PROVISIONED_MARKER,
    )
}

/// The Lima instance definition `isb machine init` starts.
pub fn render_lima_yaml(c: &LimaConfig) -> String {
    let home = c.home.to_string_lossy();
    let dir = c.home.join(".isb/machine").join(&c.name);
    let sock = |f: &str| q(&dir.join(f).to_string_lossy());
    let script = provision_script(c)
        .lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"# Generated by `isb machine init {name}`; isb owns this file.
# An Ubuntu 24.04 VM running incus, for isb on macOS.
minimumLimaVersion: 2.0.0
base:
- template:_images/ubuntu-24.04
vmType: vz
cpus: {cpus}
memory: {memory}
disk: {disk}
user:
  name: {user}
  uid: {uid}
# The Mac home at the same path, so bind sources resolve identically.
mounts:
- location: {home}
  mountPoint: {home}
  writable: true
mountType: virtiofs
containerd:
  system: false
  user: false
env:
  ISB_IDMAP_HOST_UID: "{uid}"
  ISB_IDMAP_HOST_GID: "{gid}"
  ISB_SERVE_SOCKET: {guest_serve}
provision:
- mode: system
  script: |
{script}
probes:
- mode: readiness
  description: incus installed and initialised
  script: |
    #!/bin/bash
    set -eu -o pipefail
    if ! timeout 30s bash -c "until test -e {marker}; do sleep 2; done"; then
      echo >&2 "incus is not set up yet"
      exit 1
    fi
  hint: See /var/log/cloud-init-output.log in the guest (`isb machine ssh {name}`).
portForwards:
- guestSocket: {guest_incus}
  hostSocket: {incus_sock}
- guestSocket: {guest_serve}
  hostSocket: {serve_sock}
- guestIP: 127.0.0.1
  guestPort: {serve_port}
  hostIP: 127.0.0.1
  hostPort: {serve_port}
# Lima's built-in last rule forwards every other guest loopback listener
# (published ports, the stack balancer) to the same port on the Mac.
"#,
        name = c.name,
        cpus = c.cpus,
        memory = q(&c.memory),
        disk = q(&c.disk),
        user = q(&c.user),
        uid = c.uid,
        gid = c.gid,
        home = q(&home),
        guest_serve = q(GUEST_SERVE_SOCKET),
        guest_incus = q(GUEST_INCUS_SOCKET),
        incus_sock = sock("incus.sock"),
        serve_sock = sock("serve.sock"),
        serve_port = SERVE_LISTEN.rsplit(':').next().unwrap_or("8092"),
        marker = PROVISIONED_MARKER,
    )
}

/// The `isb serve` unit in the guest. `@HOME@` is filled in there.
pub fn render_guest_unit(user: &str, uid: u32, gid: u32) -> String {
    format!(
        "[Unit]
Description=isb serve (managed by isb machine)
After=network-online.target incus.socket
Wants=network-online.target incus.socket

[Service]
Type=simple
User={user}
SupplementaryGroups=incus-admin
WorkingDirectory=@HOME@
Environment=HOME=@HOME@
Environment=ISB_SERVE_SOCKET={GUEST_SERVE_SOCKET}
Environment=ISB_SERVE_LISTEN={SERVE_LISTEN}
Environment=ISB_IDMAP_HOST_UID={uid}
Environment=ISB_IDMAP_HOST_GID={gid}
RuntimeDirectory=isb
RuntimeDirectoryMode=0700
ExecStart=/usr/local/bin/isb serve
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"
    )
}

/// Install the staged binary and the unit, then (re)start the daemon. Runs as
/// root in the guest.
fn guest_setup_script(staged: &Path, unit: &str) -> String {
    format!(
        r#"set -euo pipefail
install -m 0755 {staged} /usr/local/bin/isb
home=$(getent passwd "$SUDO_USER" | cut -d: -f6)
cat > /etc/systemd/system/isb.service.tmp <<'ISB_UNIT_EOF'
{unit}ISB_UNIT_EOF
sed "s|@HOME@|$home|g" /etc/systemd/system/isb.service.tmp > /etc/systemd/system/isb.service
rm /etc/systemd/system/isb.service.tmp
systemctl daemon-reload
systemctl enable isb.service
systemctl restart isb.service
"#,
        staged = shell_quote(&staged.to_string_lossy()),
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// limactl

fn limactl() -> Command {
    let mut c = Command::new("limactl");
    c.stdin(Stdio::null());
    c
}

fn missing_lima(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::invalid(
            "limactl not found: isb machine runs incus in a Lima VM; install Lima with \
             `brew install lima` (https://lima-vm.io)",
        )
    } else {
        Error::Io(e)
    }
}

/// Run limactl, capturing its output; fail with its stderr.
fn limactl_output(args: &[&str]) -> Result<String> {
    let out = limactl().args(args).output().map_err(missing_lima)?;
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("limactl {}", args.join(" ")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run limactl with its output on our stderr (progress the user should see).
fn limactl_passthrough(args: &[&str]) -> Result<()> {
    let st = limactl()
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(missing_lima)?;
    if !st.success() {
        return Err(Error::OperationFailed {
            step: format!("limactl {}", args.join(" ")),
            message: format!("exited with {st}"),
        });
    }
    Ok(())
}

/// `limactl version 2.2.0` → (2, 2).
fn parse_lima_version(s: &str) -> Option<(u32, u32)> {
    let v = s.split_whitespace().last()?.trim_start_matches('v');
    let mut it = v.split(['.', '-']);
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

fn check_lima_version() -> Result<()> {
    let out = limactl_output(&["--version"])?;
    match parse_lima_version(out.trim()) {
        Some((major, _)) if major >= 2 => Ok(()),
        Some(_) => Err(Error::invalid(format!(
            "{}: isb machine needs Lima 2.0 or later (`brew upgrade lima`)",
            out.trim()
        ))),
        None => Err(Error::invalid(format!(
            "cannot read the Lima version from {:?}",
            out.trim()
        ))),
    }
}

/// The Lima instance, from `limactl list --json` (one object per line).
fn lima_instance(name: &str) -> Result<Option<Value>> {
    let out = limactl_output(&["list", "--json"])?;
    Ok(out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["name"] == name))
}

fn require_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::invalid(
            "isb machine is for macOS, where incus cannot run natively; on Linux, install \
             incus on the host (https://linuxcontainers.org/incus/docs/main/installing/)",
        ))
    }
}

fn require_ours(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let d = dir(name)?;
    if !d.join("lima.yaml").exists() {
        return Err(Error::NotFound(format!(
            "machine {name} ({} has no lima.yaml; create it with `isb machine init {name}`)",
            d.display()
        )));
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// init / start / stop / rm / status

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub name: String,
    pub cpus: u32,
    pub memory: String,
    pub disk: String,
    /// A Linux isb binary for the guest; default: this version's release asset.
    pub isb_binary: Option<PathBuf>,
    /// Deadline for the first boot (image download, incus install).
    pub timeout: Duration,
}

impl Default for InitOptions {
    fn default() -> Self {
        InitOptions {
            name: DEFAULT_NAME.into(),
            cpus: 4,
            memory: "4GiB".into(),
            disk: "10GiB".into(),
            isb_binary: None,
            timeout: Duration::from_secs(20 * 60),
        }
    }
}

fn mac_ids() -> (u32, u32) {
    (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    )
}

/// Create and start the machine, install incus and the guest's `isb serve`.
/// `log` receives one progress line per step.
pub fn init(opts: &InitOptions, log: &dyn Fn(&str)) -> Result<Status> {
    require_macos()?;
    validate_name(&opts.name)?;
    check_size("--memory", &opts.memory)?;
    check_size("--disk", &opts.disk)?;
    if opts.cpus == 0 {
        return Err(Error::invalid("--cpus must be at least 1"));
    }
    check_lima_version()?;
    if lima_instance(&opts.name)?.is_some() {
        return Err(Error::invalid(format!(
            "a Lima instance named {} already exists (`limactl list`); remove it with \
             `isb machine rm {0}` if isb created it, or pick another name",
            opts.name
        )));
    }
    let home = home()?;
    let d = dir(&opts.name)?;
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)?;
    }

    let staged = d.join("isb-linux");
    match &opts.isb_binary {
        Some(p) => {
            log(&format!("using guest isb {}", p.display()));
            stage_binary(p, &staged)?;
        }
        None => {
            let ver = env!("CARGO_PKG_VERSION");
            log(&format!("downloading isb {ver} for Linux"));
            download_release(ver, std::env::consts::ARCH, &d, &staged)?;
        }
    }

    let (uid, gid) = mac_ids();
    let user = guest_user(&std::env::var("USER").unwrap_or_default());
    let cfg = LimaConfig {
        name: opts.name.clone(),
        cpus: opts.cpus,
        memory: opts.memory.clone(),
        disk: opts.disk.clone(),
        home,
        user: user.clone(),
        uid,
        gid,
    };
    let yaml = d.join("lima.yaml");
    std::fs::write(&yaml, render_lima_yaml(&cfg))?;

    log(&format!(
        "starting Lima instance {} (first boot downloads Ubuntu and installs incus)",
        opts.name
    ));
    let timeout = format!("--timeout={}s", opts.timeout.as_secs());
    let name_arg = format!("--name={}", opts.name);
    let yaml_s = yaml.to_string_lossy().into_owned();
    limactl_passthrough(&["start", &name_arg, "--tty=false", &timeout, &yaml_s])?;

    log("installing isb serve in the machine");
    guest_shell_root(
        &opts.name,
        &guest_setup_script(&staged, &render_guest_unit(&user, uid, gid)),
    )?;
    wait_ready(&opts.name, Duration::from_secs(90))?;
    status(&opts.name)
}

/// Run a bash script as root in the guest, fed on stdin.
fn guest_shell_root(name: &str, script: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("limactl")
        .args(["shell", "--workdir", "/", name, "sudo", "bash", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(missing_lima)?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(script.as_bytes())?;
    let st = child.wait()?;
    if !st.success() {
        return Err(Error::OperationFailed {
            step: format!("guest setup in machine {name}"),
            message: format!("exited with {st}"),
        });
    }
    Ok(())
}

fn stage_binary(src: &Path, dst: &Path) -> Result<()> {
    let mut magic = [0u8; 4];
    std::fs::File::open(src)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|e| Error::invalid(format!("--isb-binary {}: {e}", src.display())))?;
    if &magic != b"\x7fELF" {
        return Err(Error::invalid(format!(
            "--isb-binary {}: not a Linux (ELF) binary; the guest needs the \
             {}-unknown-linux-musl build",
            src.display(),
            std::env::consts::ARCH
        )));
    }
    std::fs::copy(src, dst)?;
    set_mode(dst, 0o755)
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// The release asset name for a version and architecture.
pub fn release_asset(version: &str, arch: &str) -> String {
    format!("isb-v{version}-{arch}-unknown-linux-musl")
}

fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let step = || format!("download {url}");
    let mut resp = agent.get(url).call().map_err(|e| Error::OperationFailed {
        step: step(),
        message: e.to_string(),
    })?;
    resp.body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| Error::OperationFailed {
            step: step(),
            message: e.to_string(),
        })
}

/// Download the release tarball, check it against SHA256SUMS, unpack the binary.
fn download_release(version: &str, arch: &str, dir: &Path, dst: &Path) -> Result<()> {
    let asset = release_asset(version, arch);
    let base = format!("{RELEASES}/v{version}");
    let sums =
        String::from_utf8_lossy(&fetch(&format!("{base}/SHA256SUMS"), 1 << 20)?).into_owned();
    let want = sums
        .lines()
        .find_map(|l| {
            let (h, f) = l.split_once(char::is_whitespace)?;
            (f.trim().trim_start_matches('*') == format!("{asset}.tar.gz")).then(|| h.to_string())
        })
        .ok_or_else(|| {
            Error::invalid(format!(
                "release v{version} has no {asset}.tar.gz; pass a Linux build with --isb-binary"
            ))
        })?;
    let tarball = fetch(&format!("{base}/{asset}.tar.gz"), 256 << 20)?;
    let got = hex(ring::digest::digest(&ring::digest::SHA256, &tarball).as_ref());
    if !got.eq_ignore_ascii_case(&want) {
        return Err(Error::invalid(format!(
            "{asset}.tar.gz: sha256 {got} does not match SHA256SUMS ({want})"
        )));
    }
    let tgz = dir.join(format!("{asset}.tar.gz"));
    std::fs::write(&tgz, &tarball)?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(dir)
        .arg(format!("{asset}/isb"))
        .stdin(Stdio::null())
        .output()?;
    let _ = std::fs::remove_file(&tgz);
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("unpack {asset}.tar.gz"),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    std::fs::rename(dir.join(&asset).join("isb"), dst)?;
    let _ = std::fs::remove_dir_all(dir.join(&asset));
    set_mode(dst, 0o755)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Wait until both forwarded sockets answer.
fn wait_ready(name: &str, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        let (incus, serve) = (probe_incus(name), probe_serve(name));
        match (&incus, &serve) {
            (Ok(_), Ok(())) => return Ok(()),
            _ if started.elapsed() >= timeout => {
                let why = incus
                    .err()
                    .map(|e| format!("incus: {e}"))
                    .or(serve.err().map(|e| format!("isb serve: {e}")))
                    .unwrap_or_default();
                return Err(Error::OperationFailed {
                    step: format!("wait for machine {name}"),
                    message: format!(
                        "not ready after {timeout:?} ({why}); look inside with `isb machine ssh {name}`"
                    ),
                });
            }
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

fn probe_incus(name: &str) -> Result<String> {
    let mut c = Client::with_socket(incus_socket(name)?);
    c.timeouts = Timeouts {
        request: Duration::from_secs(5),
        ..Timeouts::default()
    };
    let info = c.server_info()?;
    Ok(info
        .pointer("/environment/server_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

fn probe_serve(name: &str) -> Result<()> {
    crate::server::client::list_tools(&serve_socket(name)?, Duration::from_secs(5)).map(|_| ())
}

/// Start a stopped machine (a no-op when it is running) and wait for its sockets.
pub fn start(name: &str) -> Result<()> {
    require_macos()?;
    require_ours(name)?;
    let inst = lima_instance(name)?.ok_or_else(|| {
        Error::NotFound(format!(
            "Lima instance {name} (its files are in {}; remove them with `isb machine rm {name}`)",
            dir(name)
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        ))
    })?;
    if inst["status"] != "Running" {
        limactl_passthrough(&["start", "--tty=false", name])?;
    }
    wait_ready(name, Duration::from_secs(90))
}

pub fn stop(name: &str) -> Result<()> {
    require_macos()?;
    require_ours(name)?;
    match lima_instance(name)? {
        Some(i) if i["status"] == "Running" => limactl_passthrough(&["stop", name]),
        _ => Ok(()),
    }
}

/// Delete the VM, its files and a LaunchAgent that starts it.
pub fn remove(name: &str) -> Result<()> {
    require_macos()?;
    let d = require_ours(name)?;
    if lima_instance(name)?.is_some() {
        limactl_passthrough(&["delete", "--force", name])?;
    }
    if launch_agent_machine()?.as_deref() == Some(name) {
        uninstall_launch_agent()?;
    }
    std::fs::remove_dir_all(&d)?;
    Ok(())
}

/// What `isb machine status` reports.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub name: String,
    /// `Running`, `Stopped`, ... as Lima reports it; `Missing` with no instance.
    pub state: String,
    pub cpus: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub arch: Option<String>,
    pub lima_dir: Option<String>,
    pub incus_socket: PathBuf,
    /// incus's version when its socket answers.
    pub incus_version: Option<String>,
    pub incus_error: Option<String>,
    pub serve_socket: PathBuf,
    pub serve_ok: bool,
    pub serve_listen: String,
    /// Whether isb uses this machine without `INCUS_SOCKET`/`ISB_SERVE_SOCKET`.
    pub default: bool,
}

pub fn status(name: &str) -> Result<Status> {
    require_macos()?;
    require_ours(name)?;
    let inst = lima_instance(name)?;
    let running = inst.as_ref().is_some_and(|i| i["status"] == "Running");
    let (incus_version, incus_error) = if running {
        match probe_incus(name) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };
    let field = |k: &str| inst.as_ref().and_then(|i| i[k].as_u64());
    Ok(Status {
        name: name.to_string(),
        state: inst
            .as_ref()
            .and_then(|i| i["status"].as_str())
            .unwrap_or("Missing")
            .to_string(),
        cpus: field("cpus"),
        memory_bytes: field("memory"),
        disk_bytes: field("disk"),
        arch: inst
            .as_ref()
            .and_then(|i| i["arch"].as_str())
            .map(String::from),
        lima_dir: inst
            .as_ref()
            .and_then(|i| i["dir"].as_str())
            .map(String::from),
        incus_socket: incus_socket(name)?,
        incus_version,
        incus_error,
        serve_socket: serve_socket(name)?,
        serve_ok: running && probe_serve(name).is_ok(),
        serve_listen: SERVE_LISTEN.to_string(),
        default: name == DEFAULT_NAME,
    })
}

/// `limactl shell NAME [argv]`, for the caller to exec.
pub fn shell_command(name: &str, argv: &[String]) -> Result<Command> {
    require_macos()?;
    require_ours(name)?;
    let mut c = Command::new("limactl");
    c.arg("shell").arg(name).args(argv);
    Ok(c)
}

// ---------------------------------------------------------------------------
// LaunchAgent: `isb serve install` on macOS

fn launch_agent_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent: run `isb machine start NAME` at login. `path` is the PATH
/// it runs with, which must find `limactl`.
pub fn render_launch_agent(exe: &Path, name: &str, path: &str, log: &Path) -> String {
    let s = |v: &str| format!("<string>{}</string>", xml_escape(v));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `isb serve install`: starts the isb machine, where isb serve runs, at login. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  {label}
  <key>ProgramArguments</key>
  <array>
    {exe}
    <string>machine</string>
    <string>start</string>
    {name}
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    {path}
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  {log}
  <key>StandardErrorPath</key>
  {log}
</dict>
</plist>
"#,
        label = s(LAUNCH_AGENT_LABEL),
        exe = s(&exe.to_string_lossy()),
        name = s(name),
        path = s(path),
        log = s(&log.to_string_lossy()),
    )
}

/// The machine an installed LaunchAgent starts, if there is one.
fn launch_agent_machine() -> Result<Option<String>> {
    let Ok(text) = std::fs::read_to_string(launch_agent_path()?) else {
        return Ok(None);
    };
    // The argument after `start` in ProgramArguments.
    let mut it = text
        .split("<string>")
        .skip(1)
        .map(|s| s.split('<').next().unwrap_or(""));
    while let Some(v) = it.next() {
        if v == "start" {
            return Ok(it.next().map(String::from));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchAgentInstall {
    pub plist: PathBuf,
    pub exe: PathBuf,
    pub machine: String,
    pub serve_socket: PathBuf,
    pub health_url: String,
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()?)
}

/// Write the LaunchAgent, load it (which starts the machine), and wait for the
/// daemon in the machine to answer.
pub fn install_launch_agent(name: &str) -> Result<LaunchAgentInstall> {
    require_macos()?;
    require_ours(name)?;
    let exe = std::env::current_exe()?.canonicalize()?;
    let limactl_dir = find_in_path("limactl").and_then(|p| p.parent().map(Path::to_path_buf));
    let mut path: Vec<String> = Vec::new();
    if let Some(d) = limactl_dir {
        path.push(d.to_string_lossy().into_owned());
    }
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        if !path.iter().any(|p| p == d) {
            path.push(d.into());
        }
    }
    let plist = launch_agent_path()?;
    let log = dir(name)?.join("launchd.log");
    let text = render_launch_agent(&exe, name, &path.join(":"), &log);
    if let Some(d) = plist.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&plist, text)?;

    let domain = format!("gui/{}", rustix::process::getuid().as_raw());
    let target = format!("{domain}/{LAUNCH_AGENT_LABEL}");
    // Reload: bootout fails harmlessly when it is not loaded.
    let _ = launchctl(&["bootout", &target]);
    let plist_s = plist.to_string_lossy().into_owned();
    let out = launchctl(&["bootstrap", &domain, &plist_s])?;
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("launchctl bootstrap {domain} {plist_s}"),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    wait_ready(name, Duration::from_secs(300)).map_err(|e| Error::OperationFailed {
        step: format!("start machine {name} from {LAUNCH_AGENT_LABEL}"),
        message: format!("{e}; its log is {}", log.display()),
    })?;
    Ok(LaunchAgentInstall {
        plist,
        exe,
        machine: name.to_string(),
        serve_socket: serve_socket(name)?,
        health_url: format!("http://{SERVE_LISTEN}/healthz"),
    })
}

/// Unload and delete the LaunchAgent (a no-op without one).
pub fn uninstall_launch_agent() -> Result<bool> {
    let plist = launch_agent_path()?;
    if !plist.exists() {
        return Ok(false);
    }
    let target = format!(
        "gui/{}/{LAUNCH_AGENT_LABEL}",
        rustix::process::getuid().as_raw()
    );
    let _ = launchctl(&["bootout", &target]);
    std::fs::remove_file(&plist)?;
    Ok(true)
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(bin))
            .find(|c| c.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LimaConfig {
        LimaConfig {
            name: "isb".into(),
            cpus: 4,
            memory: "4GiB".into(),
            disk: "10GiB".into(),
            home: "/Users/me".into(),
            user: "me".into(),
            uid: 501,
            gid: 20,
        }
    }

    #[test]
    fn lima_yaml() {
        let y = render_lima_yaml(&cfg());
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&y).expect("valid YAML");
        assert_eq!(v["cpus"], 4);
        assert_eq!(v["memory"], "4GiB");
        assert_eq!(v["disk"], "10GiB");
        assert_eq!(v["vmType"], "vz");
        assert_eq!(v["user"]["uid"], 501);
        assert_eq!(v["mounts"][0]["location"], "/Users/me");
        assert_eq!(v["mounts"][0]["mountPoint"], "/Users/me");
        assert_eq!(v["mounts"][0]["writable"], true);
        assert_eq!(v["env"]["ISB_IDMAP_HOST_GID"], "20");
        let pf = &v["portForwards"];
        assert_eq!(pf[0]["guestSocket"], "/var/lib/incus/unix.socket");
        assert_eq!(pf[0]["hostSocket"], "/Users/me/.isb/machine/isb/incus.sock");
        assert_eq!(pf[1]["guestSocket"], "/run/isb/serve.sock");
        assert_eq!(pf[1]["hostSocket"], "/Users/me/.isb/machine/isb/serve.sock");
        assert_eq!(pf[2]["guestPort"], 8092);
        assert_eq!(pf[2]["hostPort"], 8092);
        let script = v["provision"][0]["script"].as_str().unwrap();
        assert!(script.starts_with("#!/bin/bash\n"), "{script}");
        assert!(script.contains("root:501:1"));
        assert!(script.contains("root:20:1"));
        assert!(script.contains("SocketUser=me"));
        assert!(script.contains("pkgs.zabbly.com/incus/stable"));
        assert!(
            v["probes"][0]["script"]
                .as_str()
                .unwrap()
                .contains(PROVISIONED_MARKER)
        );
        // A home with odd characters stays one YAML scalar.
        let mut c = cfg();
        c.home = "/Users/a \"b\": c".into();
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&render_lima_yaml(&c)).unwrap();
        assert_eq!(v["mounts"][0]["location"], "/Users/a \"b\": c");
    }

    #[test]
    fn names_sizes_users() {
        assert!(validate_name("isb").is_ok());
        assert!(validate_name("dev-2").is_ok());
        for bad in ["", "-x", "Isb", "a/b", "a b", &"x".repeat(33)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        for ok in ["4GiB", "512MiB", "10G", "1.5GiB", "100"] {
            assert!(check_size("x", ok).is_ok(), "{ok}");
        }
        for bad in ["", "GiB", "4 GiB", "4gigs", "-1G", "4GiB\n"] {
            assert!(check_size("x", bad).is_err(), "{bad}");
        }
        assert_eq!(guest_user("stephan"), "stephan");
        assert_eq!(guest_user("Stephan"), "lima");
        assert_eq!(guest_user("a.b"), "lima");
        assert_eq!(guest_user(""), "lima");
    }

    #[test]
    fn lima_version() {
        assert_eq!(parse_lima_version("limactl version 2.2.0"), Some((2, 2)));
        assert_eq!(parse_lima_version("limactl version v1.0.7"), Some((1, 0)));
        assert_eq!(
            parse_lima_version("limactl version 2.0.0-beta.1"),
            Some((2, 0))
        );
        assert_eq!(parse_lima_version("nonsense"), None);
    }

    #[test]
    fn guest_unit_and_setup() {
        let u = render_guest_unit("me", 501, 20);
        assert!(u.contains("\nUser=me\n"));
        assert!(u.contains("\nEnvironment=ISB_SERVE_SOCKET=/run/isb/serve.sock\n"));
        assert!(u.contains("\nEnvironment=ISB_SERVE_LISTEN=127.0.0.1:8092\n"));
        assert!(u.contains("\nEnvironment=ISB_IDMAP_HOST_UID=501\n"));
        assert!(u.contains("\nRuntimeDirectory=isb\n"));
        let s = guest_setup_script(Path::new("/Users/me/.isb/machine/isb/isb-linux"), &u);
        assert!(
            s.contains("install -m 0755 '/Users/me/.isb/machine/isb/isb-linux' /usr/local/bin/isb")
        );
        assert!(
            s.contains("\nISB_UNIT_EOF\n"),
            "heredoc terminator on its own line"
        );
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    #[test]
    fn launch_agent() {
        let p = render_launch_agent(
            Path::new("/opt/isb & co/isb"),
            "isb",
            "/opt/homebrew/bin:/usr/bin",
            Path::new("/Users/me/.isb/machine/isb/launchd.log"),
        );
        assert!(p.contains("<string>dev.isb.machine</string>"));
        assert!(p.contains("<string>/opt/isb &amp; co/isb</string>"));
        assert!(p.contains(
            "<string>machine</string>\n    <string>start</string>\n    <string>isb</string>"
        ));
        assert!(p.contains("<key>RunAtLoad</key>\n  <true/>"));
    }

    #[test]
    fn asset_names() {
        assert_eq!(
            release_asset("0.7.0", "aarch64"),
            "isb-v0.7.0-aarch64-unknown-linux-musl"
        );
    }
}
