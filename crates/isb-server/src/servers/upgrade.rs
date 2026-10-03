//! Upgrading agents: `isb server upgrade` (docs/guides/servers.md#upgrading-servers).
//!
//! The control plane hands a server a new isb binary, and a root helper on
//! the box swaps it in:
//!
//! - **an SSH server** receives it over the mTLS channel the control plane
//!   already holds (`/internal/v1/upgrade/chunk`, then `.../apply`, which
//!   checks the SHA-256 and the architecture and stages it). The bootstrap
//!   key is long gone, and keeping one would be a standing root credential
//!   on the control plane: the channel that already carries every call is
//!   the one that carries the binary;
//! - **a dedicated VM** gets it through the incus API, as at creation (file
//!   push, then exec), so even a VM made before the helper existed is
//!   upgraded, the helper installed on the way.
//!
//! Either way the helper (`isb-agent-upgrade.path` and `.service`, run as
//! root) checks the hash again on its own copy, makes sure the binary runs,
//! keeps the old one as `isb.prev`, replaces `/usr/local/bin/isb`
//! atomically and restarts the agent. It then waits [`CONFIRM_WINDOW_S`] for
//! the control plane's confirmation that the new agent answers over mTLS
//! with the new build (`/internal/v1/upgrade/confirm`); without it, it puts
//! the previous binary back and restarts again. The box rolls back, not the
//! control plane, because an agent that does not come back cannot be told
//! anything.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::bootstrap::{AGENT_HOME, AGENT_UNIT, AGENT_USER, elf_arch, sha256_hex};
use super::{AgentClient, Servers, health, store};
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions, Stdin};
use crate::sandbox::Sandbox;
use crate::stack::now_secs;

/// The control plane and agent wire, as this build speaks it. 2: SSH
/// forwarding, the upgrade routes, and `protocol`/`build`/`arch` in the
/// heartbeat. An agent that does not say is 1.
pub const PROTOCOL: u64 = 2;
/// The oldest agent this control plane forwards calls to.
pub const MIN_PROTOCOL: u64 = 1;
/// The oldest agent that takes forwarded SSH sessions.
pub const SSH_PROTOCOL: u64 = 2;

/// Upload size per request: under the agent's 4 MiB body limit.
pub const CHUNK: usize = 3 << 20;
/// No isb build is anywhere near this.
pub const MAX_BINARY: u64 = 512 << 20;
/// How long the helper waits for the control plane's confirmation before
/// it restores the previous binary.
pub const CONFIRM_WINDOW_S: u64 = 120;
/// How long the control plane waits for the new agent: inside the
/// helper's window, so a late answer is not confirmed after a rollback.
const COME_BACK: Duration = Duration::from_secs(100);
/// How long it then waits to see the rollback reported.
const ROLLBACK_SEEN: Duration = Duration::from_secs(90);

pub const HELPER: &str = "/usr/local/lib/isb/agent-upgrade";
pub const HELPER_UNIT: &str = "isb-agent-upgrade";

/// Where an agent stages an upgrade: `<state>/upgrade`.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade")
}

/// The same on a box the control plane bootstrapped.
fn box_dir() -> String {
    format!("{AGENT_HOME}/state/upgrade")
}

/// SHA-256 of this process's own executable: which build runs, whatever
/// its version says. Empty when it cannot be read.
pub fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| file_sha256(Path::new("/proc/self/exe")).unwrap_or_default())
}

fn file_sha256(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(crate::machine::hex(ctx.finish().as_ref()))
}

/// The protocol a heartbeat says the agent speaks.
pub fn protocol_of(hb: &Value) -> u64 {
    hb["protocol"].as_u64().unwrap_or(1)
}

/// Whether this control plane may forward to an agent that last said
/// `hb` (`Null`: not heard from yet in this run, so not refused).
pub fn compatible(name: &str, hb: &Value, need: u64) -> Result<()> {
    if hb.is_null() {
        return Ok(());
    }
    let p = protocol_of(hb);
    let ver = hb["isb"].as_str().unwrap_or("unknown");
    if p > PROTOCOL {
        return Err(Error::invalid(format!(
            "server {name} runs isb {ver} (agent protocol {p}), newer than this control plane understands (protocol {PROTOCOL}): upgrade the control plane"
        )));
    }
    if p < need {
        return Err(Error::invalid(format!(
            "server {name} runs isb {ver} (agent protocol {p}); this needs protocol {need}: upgrade it with `isb server upgrade {name}`"
        )));
    }
    Ok(())
}

/// The heartbeat's own part: the wire, the build, and the upgrade helper.
pub fn heartbeat_fields(state_dir: &Path) -> Value {
    json!({
        "protocol": PROTOCOL,
        "build": build_id(),
        "arch": std::env::consts::ARCH,
        "upgrade": status(&dir(state_dir)),
    })
}

/// Whether the helper is installed, and what its last run did.
pub fn status(dir: &Path) -> Value {
    let helper = Path::new(&format!("/etc/systemd/system/{HELPER_UNIT}.path")).exists();
    let last = std::fs::read(dir.join("result"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    json!({"helper": helper, "last": last})
}

// ---- the root helper, installed on the box ----

/// The helper itself: `sh`, run as root by `isb-agent-upgrade.service`
/// when the agent (or, for a dedicated VM, the control plane through
/// incus) writes `request`.
pub fn helper_script() -> String {
    let dir = box_dir();
    let window = CONFIRM_WINDOW_S;
    format!(
        r#"#!/bin/sh
# Installs the isb binary the control plane staged for this agent, restarts
# the agent, and restores the previous binary unless the control plane
# confirms within {window} s that the new agent answers. Written by isb;
# run by {HELPER_UNIT}.service.
set -u
d={dir}
bin=/usr/local/bin/isb
[ -f "$d/request" ] || exit 0
rm -f "$d/request" "$d/confirmed"
want=$(tr -dc 0-9a-f < "$d/sha256" 2>/dev/null | head -c 64)
result() {{
  printf '{{"state":"%s","sha256":"%s","at":%s,"message":"%s"}}\n' "$1" "$want" "$(date +%s)" "$2" > "$d/result.new"
  chown {user}:{user} "$d/result.new" 2>/dev/null
  mv -f "$d/result.new" "$d/result"
  echo "isb-agent-upgrade: $1: $2"
}}
[ "${{#want}}" -eq 64 ] || {{ result failed "no sha256 was staged"; exit 1; }}
# Check our own copy, so what is checked is what is installed.
tmp=$(mktemp /usr/local/bin/.isb.XXXXXX) || {{ result failed "mktemp failed"; exit 1; }}
if ! cp "$d/isb.new" "$tmp" || ! chmod 0755 "$tmp"; then
  rm -f "$tmp"; result failed "the staged binary could not be copied"; exit 1
fi
if ! echo "$want  $tmp" | sha256sum -c --quiet - >/dev/null 2>&1; then
  rm -f "$tmp"; result failed "the staged binary does not match its sha256"; exit 1
fi
if ! runuser -u {user} -- "$tmp" --version >/dev/null 2>&1; then
  rm -f "$tmp"; result failed "the staged binary does not run"; exit 1
fi
cp -p "$bin" "$bin.prev" || {{ rm -f "$tmp"; result failed "could not keep the previous binary"; exit 1; }}
mv -f "$tmp" "$bin"
result restarting "installed; waiting for the control plane to confirm"
systemctl reset-failed {unit} 2>/dev/null
systemctl restart {unit}
i=0
while [ "$i" -lt {window} ]; do
  if [ -f "$d/confirmed" ] && [ "$(tr -dc 0-9a-f < "$d/confirmed")" = "$want" ]; then
    rm -f "$d/isb.new" "$d/isb.new.part" "$d/confirmed"
    result done "upgraded"
    exit 0
  fi
  sleep 1
  i=$((i + 1))
done
cp -p "$bin.prev" "$bin.rollback" && mv -f "$bin.rollback" "$bin"
# A binary that keeps exiting may have hit the unit's start limit.
systemctl reset-failed {unit} 2>/dev/null
systemctl restart {unit}
result rolled_back "the new agent was not confirmed within {window} s: the previous binary is back"
exit 1
"#,
        user = AGENT_USER,
        unit = AGENT_UNIT,
    )
}

/// Root shell that installs (or refreshes) the helper and its units;
/// idempotent. Part of every bootstrap, and run before a dedicated VM's
/// upgrade.
pub fn helper_install() -> String {
    let dir = box_dir();
    let mut s = String::new();
    s.push_str("install -d -m 0755 /usr/local/lib/isb\n");
    s.push_str(&format!("cat > {HELPER}.new <<'ISB_HELPER_EOF'\n"));
    s.push_str(&helper_script());
    s.push_str("ISB_HELPER_EOF\n");
    s.push_str(&format!(
        "chmod 0755 {HELPER}.new && mv -f {HELPER}.new {HELPER}\n"
    ));
    s.push_str(&format!(
        "cat > /etc/systemd/system/{HELPER_UNIT}.service <<'ISB_HELPER_EOF'
[Unit]
Description=Install the isb agent binary its control plane staged

[Service]
Type=oneshot
ExecStart={HELPER}
TimeoutStartSec={timeout}
ISB_HELPER_EOF
cat > /etc/systemd/system/{HELPER_UNIT}.path <<'ISB_HELPER_EOF'
[Unit]
Description=Watch for an isb agent upgrade its control plane staged

[Path]
PathExists={dir}/request

[Install]
WantedBy=multi-user.target
ISB_HELPER_EOF
[ -d {home}/state ] || install -d -o {user} -g {user} -m 0750 {home}/state
install -d -o {user} -g {user} -m 0700 {dir}
systemctl daemon-reload
systemctl enable --now {HELPER_UNIT}.path >/dev/null 2>&1
",
        timeout = CONFIRM_WINDOW_S + 180,
        home = AGENT_HOME,
        user = AGENT_USER,
    ));
    s
}

// ---- the agent's side ----

/// Take `body` at `offset` of the binary being uploaded (offset 0 starts
/// over); the size so far.
pub fn stage_chunk(dir: &Path, offset: u64, body: &[u8]) -> Result<u64> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join("isb.new.part");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(&part)?;
    let len = f.metadata()?.len();
    if offset != len {
        return Err(Error::invalid(format!(
            "upload: chunk at {offset}, but {len} bytes are staged"
        )));
    }
    if offset + body.len() as u64 > MAX_BINARY {
        return Err(Error::invalid("upload: larger than any isb binary"));
    }
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(body)?;
    Ok(offset + body.len() as u64)
}

/// The upload is complete: check it is `size` bytes with `sha256`, a
/// Linux executable for this machine, then stage it and ask the helper.
pub fn stage_apply(dir: &Path, sha256: &str, size: u64) -> Result<()> {
    let part = dir.join("isb.new.part");
    let len = std::fs::metadata(&part)
        .map_err(|_| Error::invalid("upload: nothing staged"))?
        .len();
    if len != size {
        return Err(Error::invalid(format!(
            "upload: {len} bytes staged, {size} expected"
        )));
    }
    let got = file_sha256(&part)?;
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(Error::invalid(format!(
            "upload: sha256 {got} does not match {sha256}"
        )));
    }
    let mut head = [0u8; 20];
    std::fs::File::open(&part)?.read_exact(&mut head)?;
    match elf_arch(&head) {
        Some(a) if a == std::env::consts::ARCH => {}
        Some(a) => {
            return Err(Error::invalid(format!(
                "the binary is for {a}; this server is {}",
                std::env::consts::ARCH
            )));
        }
        None => return Err(Error::invalid("the binary is not a Linux executable")),
    }
    if status(dir)["helper"] != true {
        return Err(Error::invalid(format!(
            "this server has no upgrade helper ({HELPER_UNIT}.path): it was bootstrapped by an older isb; upgrade it by hand once (docs/operations/upgrades.md)"
        )));
    }
    std::fs::rename(&part, dir.join("isb.new"))?;
    for f in ["confirmed", "result"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    std::fs::write(
        dir.join("sha256"),
        format!("{}\n", sha256.to_ascii_lowercase()),
    )?;
    std::fs::write(dir.join("request"), format!("{}\n", now_secs()))?;
    Ok(())
}

/// The control plane saw this agent answer: tell the helper to keep it,
/// if it is the build asked for.
pub fn confirm(dir: &Path, sha256: &str) -> Result<()> {
    if !build_id().eq_ignore_ascii_case(sha256) {
        return Err(Error::invalid(format!(
            "this agent runs build {}, not {sha256}",
            build_id()
        )));
    }
    std::fs::write(
        dir.join("confirmed"),
        format!("{}\n", sha256.to_ascii_lowercase()),
    )?;
    Ok(())
}

// ---- the control plane's side ----

/// What to install.
#[derive(Debug, Clone)]
pub enum Source {
    /// The control plane's own executable.
    Own,
    /// A release, checked against its `SHA256SUMS`.
    Release(String),
    /// A Linux binary on the control plane's host.
    File(PathBuf),
}

impl Servers {
    /// Upgrade one server's agent to `source`'s build and wait until it
    /// answers with it; the helper on the box rolls back otherwise.
    pub fn upgrade(&self, client: &crate::Client, name: &str, source: &Source) -> Result<Value> {
        let _busy = self.begin_upgrade(name)?;
        let rec = self.record(name)?;
        let c = self.client(name)?;
        let hb = c
            .internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT)
            .map_err(|e| {
                Error::invalid(format!(
                    "server {name} does not answer ({e}): an upgrade needs its agent running"
                ))
            })?;
        let arch = match (hb["arch"].as_str(), &rec.vm) {
            (Some(a), _) => a.to_string(),
            // A dedicated VM runs on this host.
            (None, Some(_)) => std::env::consts::ARCH.to_string(),
            (None, None) => {
                return Err(Error::invalid(format!(
                    "server {name} runs isb {} without the upgrade routes: upgrade it by hand once (docs/operations/upgrades.md)",
                    hb["isb"].as_str().unwrap_or("?")
                )));
            }
        };
        let bin = self.binary(source, &arch)?;
        let sha = sha256_hex(&bin);
        let from = json!({"isb": hb["isb"], "build": hb["build"]});
        if hb["build"].as_str() == Some(sha.as_str()) {
            return Ok(
                json!({"name": name, "upgraded": false, "from": from, "to": from,
                "note": "already runs this build"}),
            );
        }
        match &rec.vm {
            Some(v) => stage_vm(client, v, &bin, &sha)?,
            None => stage_mtls(&c, &bin, &sha)?,
        }
        let now = match come_back(&c, &sha) {
            Ok(hb) => hb,
            Err(why) => {
                let (e, seen) = rollback_error(&c, name, &sha, why);
                if let Some(hb) = seen {
                    self.observe(name, hb);
                }
                return Err(e);
            }
        };
        c.internal(
            "POST",
            "/internal/v1/upgrade/confirm",
            Some(&json!({"sha256": sha})),
            health::TIMEOUT,
        )?;
        let to = json!({"isb": now["isb"], "build": now["build"]});
        self.upgraded(name, &now);
        Ok(json!({"name": name, "upgraded": true, "from": from, "to": to}))
    }

    /// One upgrade per server at a time.
    fn begin_upgrade(&self, name: &str) -> Result<UpgradeGuard<'_>> {
        if !self.upgrading.lock().unwrap().insert(name.to_string()) {
            return Err(Error::invalid(format!(
                "server {name} is being upgraded already"
            )));
        }
        Ok(UpgradeGuard {
            set: &self.upgrading,
            name: name.to_string(),
        })
    }

    fn binary(&self, source: &Source, arch: &str) -> Result<Vec<u8>> {
        let b = match source {
            Source::Own => return super::bootstrap::own_binary(arch),
            Source::File(f) => {
                std::fs::read(f).map_err(|e| Error::invalid(format!("{}: {e}", f.display())))?
            }
            Source::Release(v) => {
                let scratch = self.dir.join(format!("tmp-upgrade-{}", now_secs()));
                std::fs::create_dir_all(&scratch)?;
                let dst = scratch.join("isb");
                let r = crate::machine::download_release(v, arch, &scratch, &dst)
                    .and_then(|()| Ok(std::fs::read(&dst)?));
                let _ = std::fs::remove_dir_all(&scratch);
                r?
            }
        };
        match elf_arch(&b) {
            Some(a) if a == arch => Ok(b),
            Some(a) => Err(Error::invalid(format!(
                "the isb binary is for {a}; the server is {arch}"
            ))),
            None => Err(Error::invalid("the isb binary is not a Linux executable")),
        }
    }

    /// Record what the server runs now.
    fn upgraded(&self, name: &str, hb: &Value) {
        {
            let mut st = self.store.lock().unwrap();
            if let Some(r) = st.servers.get_mut(name) {
                r.isb_version = hb["isb"].as_str().unwrap_or("").to_string();
                let _ = st.save();
            }
        }
        self.observe(name, hb.clone());
    }

    /// A heartbeat seen outside the health thread.
    fn observe(&self, name: &str, hb: Value) {
        self.health
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .observe(Ok(hb), now_secs());
    }
}

struct UpgradeGuard<'a> {
    set: &'a std::sync::Mutex<std::collections::BTreeSet<String>>,
    name: String,
}

impl Drop for UpgradeGuard<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.name);
    }
}

/// Send the binary over mTLS in chunks and stage it.
fn stage_mtls(c: &AgentClient, bin: &[u8], sha: &str) -> Result<()> {
    let mut offset = 0usize;
    for chunk in bin.chunks(CHUNK) {
        c.internal_bytes(
            &format!("/internal/v1/upgrade/chunk?offset={offset}"),
            chunk,
            Duration::from_secs(120),
        )?;
        offset += chunk.len();
    }
    c.internal(
        "POST",
        "/internal/v1/upgrade/apply",
        Some(&json!({"sha256": sha, "size": bin.len()})),
        Duration::from_secs(60),
    )?;
    Ok(())
}

/// Push the binary into a dedicated VM through incus, install the helper
/// there (a VM made before it existed has none), and stage the upgrade.
fn stage_vm(client: &crate::Client, v: &store::VmRecord, bin: &[u8], sha: &str) -> Result<()> {
    let sys = client.clone().project(&v.project);
    let upload = "/root/isb-agent.upgrade";
    sys.push_file(&v.instance, upload, bin, 0, 0, 0o600)?;
    let dir = box_dir();
    let script = format!(
        "set -eu\n{helper}echo \"{sha}  {upload}\" | sha256sum -c --quiet -\n\
         install -o {user} -g {user} -m 0600 {upload} {dir}/isb.new\nrm -f {upload} {dir}/confirmed {dir}/result\n\
         echo {sha} > {dir}/sha256\nchown {user}:{user} {dir}/sha256\ndate +%s > {dir}/request\n",
        helper = helper_install(),
        user = AGENT_USER,
    );
    let sb = Sandbox::get(&sys, &v.instance)?;
    let mut s = sb.exec_stream(
        ["sh", "-s"],
        ExecOptions::default()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .timeout(Duration::from_secs(120))
            .stdin(Stdin::Bytes(script.into_bytes())),
    )?;
    let mut out = Vec::new();
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) | ExecEvent::Stderr(b) => out.extend(b),
        }
    }
    let code = s.wait()?;
    if code != 0 {
        let text = String::from_utf8_lossy(&out);
        return Err(Error::OperationFailed {
            step: format!("stage the upgrade in VM {}", v.instance),
            message: format!("exit {code}: {}", text.trim()),
        });
    }
    Ok(())
}

/// Wait for the agent to answer with build `sha`.
fn come_back(c: &AgentClient, sha: &str) -> std::result::Result<Value, String> {
    let started = Instant::now();
    let mut last = String::from("no answer yet");
    while started.elapsed() < COME_BACK {
        match c.internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT) {
            Ok(hb) if hb["build"].as_str() == Some(sha) => return Ok(hb),
            Ok(hb) => {
                last = match hb["upgrade"]["last"]["state"].as_str() {
                    Some("failed") if hb["upgrade"]["last"]["sha256"] == sha => {
                        // The helper refused it before touching anything.
                        return Err(format!(
                            "the server's upgrade helper refused it: {}",
                            hb["upgrade"]["last"]["message"].as_str().unwrap_or("")
                        ));
                    }
                    _ => format!(
                        "still answering with build {}",
                        hb["build"].as_str().unwrap_or("?")
                    ),
                }
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(format!(
        "the agent did not answer with the new build within {}s ({last})",
        COME_BACK.as_secs()
    ))
}

/// The error for an agent that did not come back: wait to see the box's
/// helper restore the previous binary, and say so; with the heartbeat that
/// showed it.
fn rollback_error(c: &AgentClient, name: &str, sha: &str, why: String) -> (Error, Option<Value>) {
    let started = Instant::now();
    let mut rolled = None;
    if !why.contains("refused it") {
        while started.elapsed() < ROLLBACK_SEEN {
            if let Ok(hb) = c.internal("GET", "/internal/v1/heartbeat", None, health::TIMEOUT) {
                let last = &hb["upgrade"]["last"];
                if last["sha256"] == sha && last["state"] == "rolled_back" {
                    rolled = Some(hb);
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }
    let message = match &rolled {
        Some(hb) => format!(
            "{why}; the server restored its previous binary and answers again (isb {}, build {})",
            hb["isb"].as_str().unwrap_or("?"),
            short(hb["build"].as_str().unwrap_or("?"))
        ),
        None if why.contains("refused it") => why,
        None => format!(
            "{why}; its upgrade helper restores the previous binary {CONFIRM_WINDOW_S}s after the restart: check `isb server show {name}`"
        ),
    };
    let e = Error::OperationFailed {
        step: format!("upgrade server {name}"),
        message,
    };
    (e, rolled)
}

fn short(b: &str) -> &str {
    &b[..b.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_are_checked_both_ways() {
        compatible("s", &Value::Null, SSH_PROTOCOL).unwrap();
        compatible("s", &json!({"isb": "0.7.0"}), MIN_PROTOCOL).unwrap();
        let old = compatible("s", &json!({"isb": "0.7.0"}), SSH_PROTOCOL).unwrap_err();
        assert!(old.to_string().contains("isb server upgrade s"), "{old}");
        let new =
            compatible("s", &json!({"isb": "9.0.0", "protocol": PROTOCOL + 1}), 1).unwrap_err();
        assert!(
            new.to_string().contains("upgrade the control plane"),
            "{new}"
        );
        compatible("s", &json!({"protocol": PROTOCOL}), SSH_PROTOCOL).unwrap();
    }

    #[test]
    fn chunks_stage_in_order_and_apply_checks_the_hash() {
        let d = tempfile::tempdir().unwrap();
        let mut bin = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0".to_vec();
        bin.extend(match std::env::consts::ARCH {
            "aarch64" => [0xb7, 0],
            _ => [0x3e, 0],
        });
        bin.extend(vec![7u8; 100]);
        let sha = sha256_hex(&bin);
        assert_eq!(stage_chunk(d.path(), 0, &bin[..50]).unwrap(), 50);
        assert!(
            stage_chunk(d.path(), 10, &bin[50..]).is_err(),
            "out of order"
        );
        assert_eq!(
            stage_chunk(d.path(), 50, &bin[50..]).unwrap(),
            bin.len() as u64
        );
        assert!(stage_apply(d.path(), &sha, 5).is_err(), "wrong size");
        let bad = stage_apply(d.path(), &"0".repeat(64), bin.len() as u64).unwrap_err();
        assert!(bad.to_string().contains("does not match"), "{bad}");
        // No helper on a test machine: refused, with the way out.
        if status(d.path())["helper"] != true {
            let e = stage_apply(d.path(), &sha, bin.len() as u64).unwrap_err();
            assert!(e.to_string().contains("upgrade helper"), "{e}");
        }
        // A restart: the upload starts over.
        assert_eq!(stage_chunk(d.path(), 0, b"x").unwrap(), 1);
        assert!(confirm(d.path(), &"f".repeat(64)).is_err());
    }

    #[test]
    fn the_helper_checks_restarts_and_rolls_back() {
        let s = helper_script();
        assert!(s.contains("sha256sum -c"));
        assert!(s.contains("runuser -u isb --"));
        assert!(s.contains("systemctl restart isb-agent.service"));
        assert!(s.contains("mv -f \"$tmp\" \"$bin\""));
        assert!(s.contains("rolled_back"));
        assert!(s.contains(&format!("-lt {CONFIRM_WINDOW_S}")));
        let i = helper_install();
        assert!(i.contains("PathExists=/var/lib/isb/state/upgrade/request"));
        assert!(i.contains("systemctl enable --now isb-agent-upgrade.path"));
        // The heredoc's end marker never occurs inside it.
        assert_eq!(i.matches("\nISB_HELPER_EOF\n").count(), 3);
    }
}
