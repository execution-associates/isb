//! `isb server add`: make a fresh Linux box an isb agent over SSH.
//!
//! The SSH key is used for this and nothing after: it installs incus (from
//! Zabbly's stable channel, as `isb machine` does in its VM), the isb binary
//! (checksum checked on both ends), runs `isb host setup`, writes the
//! agent's TLS material and a systemd unit for `isb serve --agent`, and
//! optionally closes the box's firewall to everything but SSH and the agent
//! port from the control plane. From then on the control plane speaks only
//! mTLS to the agent.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::pki::Leaf;
use super::provision::Provision;
use crate::error::{Error, Result};

/// Where things go on the box.
pub const AGENT_USER: &str = "isb";
pub const AGENT_HOME: &str = "/var/lib/isb";
pub const AGENT_TLS_DIR: &str = "/etc/isb-agent";
pub const AGENT_UNIT: &str = "isb-agent.service";
pub const DEFAULT_AGENT_PORT: u16 = 7443;

/// What `isb server add` was given.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub name: String,
    /// `user@host`; the user is root or has passwordless sudo.
    pub ssh: String,
    pub ssh_port: u16,
    pub key: PathBuf,
    /// What the control plane dials, if not the SSH host.
    pub address: Option<String>,
    pub agent_port: u16,
    /// Firewall the box to SSH plus the agent port from these (ufw).
    pub allow_from: Vec<String>,
    /// A Linux isb binary to install; default the release of this version.
    pub isb_binary: Option<PathBuf>,
    pub version: Option<String>,
    /// Install this control plane's own executable (it must be a Linux
    /// build for the box's architecture).
    pub self_binary: bool,
    /// Serve the box's orgs' domains on its own 80 and 443 (`isb host
    /// setup --public-ingress`, the agent's `--ingress-http/https`).
    pub public_ingress: bool,
}

/// Server names: `[a-z0-9-]`, a letter first, at most 40 (so `vm-` and
/// the longest org name fit).
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name != "local";
    if !ok {
        return Err(Error::invalid(format!(
            "server name {name:?}: [a-z0-9-], starting with a letter, at most 40 characters, not \"local\""
        )));
    }
    Ok(())
}

/// The host part of `user@host`.
pub fn ssh_host(ssh: &str) -> Result<(&str, &str)> {
    let (u, h) = ssh
        .split_once('@')
        .ok_or_else(|| Error::invalid(format!("--ssh {ssh:?}: expected user@host")))?;
    if !host_ok(u) || !host_ok(h) {
        return Err(Error::invalid(format!("--ssh {ssh:?}: not a user@host")));
    }
    Ok((u, h))
}

/// A user or host name, or an address: nothing a shell or ssh would read
/// as more.
fn host_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// What the control plane dials: a host name or an address.
pub fn check_address(s: &str) -> Result<()> {
    if !host_ok(s) || s.contains('_') {
        return Err(Error::invalid(format!(
            "--address {s:?}: a host name or an IP address"
        )));
    }
    Ok(())
}

/// A CIDR or address for `ufw allow from`.
pub fn check_cidr(s: &str) -> Result<()> {
    let (ip, len) = s.split_once('/').unwrap_or((s, ""));
    let ok = ip.parse::<std::net::IpAddr>().is_ok()
        && (len.is_empty() || len.parse::<u8>().is_ok_and(|n| n <= 128));
    if !ok {
        return Err(Error::invalid(format!(
            "--allow-from {s:?}: an address or CIDR"
        )));
    }
    Ok(())
}

/// Runs commands on the box over SSH with exactly the given key: no agent,
/// no user ssh config, a known_hosts file of its own.
pub struct Ssh {
    target: String,
    port: u16,
    key: PathBuf,
    known_hosts: PathBuf,
}

impl Ssh {
    pub fn new(target: &str, port: u16, key: &Path, known_hosts: &Path) -> Ssh {
        Ssh {
            target: target.into(),
            port,
            key: key.into(),
            known_hosts: known_hosts.into(),
        }
    }

    fn command(&self, remote: &str) -> Command {
        let mut c = Command::new("ssh");
        c.env_remove("SSH_AUTH_SOCK")
            .args(["-F", "/dev/null", "-i"])
            .arg(&self.key)
            .args([
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ConnectTimeout=15",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
                "-o",
            ])
            .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
            .arg("-p")
            .arg(self.port.to_string())
            .arg(&self.target)
            .arg("--")
            .arg(remote);
        c
    }

    /// Run `remote` with `input` on its stdin; its stdout, or the step's
    /// failure with its stderr's tail.
    pub fn run(&self, step: &str, remote: &str, input: &[u8]) -> Result<String> {
        let mut c = self.command(remote);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| Error::OperationFailed {
            step: step.into(),
            message: format!("ssh: {e}"),
        })?;
        let mut stdin = child.stdin.take();
        let data = input.to_vec();
        let feeder = std::thread::spawn(move || {
            if let Some(s) = stdin.as_mut() {
                let _ = s.write_all(&data);
            }
            drop(stdin);
        });
        let out = child.wait_with_output()?;
        let _ = feeder.join();
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(15).collect();
            return Err(Error::OperationFailed {
                step: step.into(),
                message: format!(
                    "exit {}: {}",
                    out.status.code().unwrap_or(-1),
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                ),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `x86_64` or `aarch64` from an ELF header, if it is a Linux executable.
pub fn elf_arch(b: &[u8]) -> Option<&'static str> {
    if b.len() < 20 || &b[..4] != b"\x7fELF" {
        return None;
    }
    match u16::from_le_bytes([b[18], b[19]]) {
        0x3e => Some("x86_64"),
        0xb7 => Some("aarch64"),
        _ => None,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The agent's unit.
pub fn render_unit(port: u16, public_ingress: bool) -> String {
    let ingress = if public_ingress {
        " --ingress-http 0.0.0.0:80 --ingress-https 0.0.0.0:443"
    } else {
        ""
    };
    format!(
        "[Unit]
Description=isb agent (isb serve --agent, managed by an isb control plane)
After=network-online.target incus.socket
Wants=network-online.target incus.socket

[Service]
Type=simple
User={AGENT_USER}
SupplementaryGroups=incus-admin
WorkingDirectory={AGENT_HOME}
Environment=HOME={AGENT_HOME}
Environment=ISB_SERVE_SOCKET=/run/isb/serve.sock
RuntimeDirectory=isb
RuntimeDirectoryMode=0700
ExecStart=/usr/local/bin/isb serve --agent --agent-listen 0.0.0.0:{port} --agent-tls {AGENT_TLS_DIR} --state-dir {AGENT_HOME}/state{ingress}
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"
    )
}

/// The root script: everything idempotent, so a second `server add` (or a
/// rerun after a failure) converges. `ssh_port` stays open in the
/// firewall; a dedicated VM, bootstrapped through incus, has none.
#[allow(clippy::too_many_arguments)]
pub fn render_script(
    upload: &str,
    sha256: &str,
    ca_pem: &str,
    leaf: &Leaf,
    port: u16,
    allow_from: &[String],
    ssh_port: Option<u16>,
    public_ingress: bool,
) -> String {
    let mut fw = String::new();
    if !allow_from.is_empty() {
        fw.push_str(
            "command -v ufw >/dev/null 2>&1 || { apt-get update; apt-get install -y --no-install-recommends ufw; }\n",
        );
        if let Some(p) = ssh_port {
            fw.push_str(&format!("ufw allow {p}/tcp\n"));
        }
        for c in allow_from {
            fw.push_str(&format!(
                "ufw allow proto tcp from {} to any port {port}\n",
                shell_quote(c)
            ));
        }
        fw.push_str("ufw --force enable\n");
    }
    format!(
        r#"set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
echo "{sha256}  {upload}" | sha256sum -c --quiet -
id -u {user} >/dev/null 2>&1 || useradd --system --create-home --home-dir {home} --shell /usr/sbin/nologin {user}
uid=$(id -u {user})
# Containers get their own range, and incus may map the agent's uid 1:1
# (restricted.idmap.uid in each org's project).
changed=0
for l in root:1000000:1000000000 "root:$uid:1"; do
  for f in /etc/subuid /etc/subgid; do
    grep -qx "$l" "$f" || {{ echo "$l" >> "$f"; changed=1; }}
  done
done
if ! command -v incus >/dev/null 2>&1; then
  apt-get update
  apt-get install -y --no-install-recommends curl ca-certificates
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
elif [ "$changed" = 1 ]; then
  systemctl restart incus.service || true
fi
if [ -z "$(incus storage list --format csv 2>/dev/null)" ]; then
  incus admin init --auto
fi
usermod -aG incus-admin {user}
install -m 0755 {upload_q} /usr/local/bin/isb
rm -f {upload_q}
{fw}# The local registry, for builds; then host setup installs its CA.
runuser -u {user} -- env HOME={home} /usr/local/bin/isb registry setup --state-dir {home}/state \
  || echo "isb: the local registry could not be set up; builds on this server will fail" >&2
/usr/local/bin/isb host setup --user {user}{host_ingress}
install -d -o {user} -g {user} -m 0700 {tls}
cat > {tls}/ca.crt <<'ISB_EOF'
{ca}ISB_EOF
umask 077
cat > {tls}/tls.crt.new <<'ISB_EOF'
{cert}ISB_EOF
cat > {tls}/tls.key.new <<'ISB_EOF'
{key}ISB_EOF
mv {tls}/tls.crt.new {tls}/tls.crt
mv {tls}/tls.key.new {tls}/tls.key
chown {user}:{user} {tls}/ca.crt {tls}/tls.crt {tls}/tls.key
chmod 0644 {tls}/ca.crt
umask 022
cat > /etc/systemd/system/{unit} <<'ISB_EOF'
{unit_text}ISB_EOF
systemctl daemon-reload
systemctl enable {unit} >/dev/null 2>&1
systemctl restart {unit}
echo "incus $(incus version 2>/dev/null | tail -n1 | awk '{{print $NF}}')"
"#,
        upload_q = shell_quote(upload),
        user = AGENT_USER,
        home = AGENT_HOME,
        tls = AGENT_TLS_DIR,
        ca = ca_pem,
        cert = leaf.cert,
        key = leaf.key,
        unit = AGENT_UNIT,
        unit_text = render_unit(port, public_ingress),
        host_ingress = if public_ingress {
            " --public-ingress"
        } else {
            ""
        },
    )
}

/// Run the bootstrap, reporting each step to `p`.
pub fn run(
    o: &AddOptions,
    ssh: &Ssh,
    ca_pem: &str,
    leaf: &Leaf,
    scratch: &Path,
    p: &Provision,
) -> Result<String> {
    let (user, _) = ssh_host(&o.ssh)?;
    p.step("check");
    p.log(&format!("checking the box: ssh {}:{}", o.ssh, o.ssh_port));
    let uname = ssh.run("check the box", "uname -sm", b"")?;
    let arch = match uname.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["Linux", "x86_64"] => "x86_64",
        ["Linux", "aarch64"] => "aarch64",
        _ => {
            return Err(Error::invalid(format!(
                "server {}: isb agents run on Linux x86_64 or aarch64, not {}",
                o.name,
                uname.trim()
            )));
        }
    };
    p.log(&format!(
        "{} {arch}",
        uname.split_whitespace().next().unwrap_or("")
    ));
    if user != "root" {
        ssh.run("check sudo", "sudo -n true", b"").map_err(|_| {
            Error::invalid(format!(
                "{}: the user needs passwordless sudo (or connect as root)",
                o.ssh
            ))
        })?;
    }
    let sudo = if user == "root" { "" } else { "sudo -n " };
    p.step("binary");
    let binary = binary(o, arch, scratch, p)?;
    let sha = sha256_hex(&binary);
    p.step("upload");
    p.log(&format!(
        "uploading isb ({} MiB, sha256 {})",
        binary.len() >> 20,
        &sha[..16]
    ));
    let upload = ssh
        .run(
            "upload isb",
            "f=$(mktemp /tmp/isb-agent.XXXXXX) && cat > \"$f\" && echo \"$f\"",
            &binary,
        )?
        .trim()
        .to_string();
    if !upload.starts_with("/tmp/isb-agent.")
        || !upload
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_'))
    {
        return Err(Error::invalid(format!(
            "upload isb: unexpected path {upload:?}"
        )));
    }
    p.step("install");
    p.log("installing incus, the agent and its unit (this takes a few minutes on a fresh box)");
    let script = render_script(
        &upload,
        &sha,
        ca_pem,
        leaf,
        o.agent_port,
        &o.allow_from,
        Some(o.ssh_port),
        o.public_ingress,
    );
    let out = ssh.run(
        "install the agent",
        &format!("{sudo}bash -s"),
        script.as_bytes(),
    )?;
    Ok(out.lines().last().unwrap_or("").to_string())
}

pub fn sha256_hex(b: &[u8]) -> String {
    crate::machine::hex(ring::digest::digest(&ring::digest::SHA256, b).as_ref())
}

/// This process's own executable, when it is a Linux build for `arch`.
pub fn own_binary(arch: &str) -> Result<Vec<u8>> {
    let b = std::fs::read("/proc/self/exe")
        .map_err(|e| Error::invalid(format!("this control plane's own binary: {e}")))?;
    match elf_arch(&b) {
        Some(a) if a == arch => Ok(b),
        Some(a) => Err(Error::invalid(format!(
            "this control plane's binary is for {a}; the box is {arch}"
        ))),
        None => Err(Error::invalid(
            "this control plane's binary is not a Linux executable",
        )),
    }
}

/// The binary to install: this process's own, a file given, or a release
/// (checked against its SHA256SUMS); checked to be for `arch`.
fn binary(o: &AddOptions, arch: &str, scratch: &Path, p: &Provision) -> Result<Vec<u8>> {
    if o.self_binary {
        p.log(&format!(
            "using this control plane's own binary (isb {})",
            env!("CARGO_PKG_VERSION")
        ));
        return own_binary(arch);
    }
    let b = match &o.isb_binary {
        Some(f) => {
            p.log(&format!("using {}", f.display()));
            std::fs::read(f).map_err(|e| Error::invalid(format!("{}: {e}", f.display())))?
        }
        None => {
            let ver = o
                .version
                .clone()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
            p.log(&format!("downloading isb v{ver} for {arch}"));
            let dst = scratch.join("isb");
            crate::machine::download_release(&ver, arch, scratch, &dst)?;
            std::fs::read(&dst)?
        }
    };
    match elf_arch(&b) {
        Some(a) if a == arch => Ok(b),
        Some(a) => Err(Error::invalid(format!(
            "the isb binary is for {a}; the box is {arch}"
        ))),
        None => Err(Error::invalid("the isb binary is not a Linux executable")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_targets_are_checked() {
        validate_name("hel-1").unwrap();
        validate_name(&format!("vm-{}", "a".repeat(31))).unwrap();
        for bad in ["", "Local", "local", "1box", "a_b", &"a".repeat(41)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        assert_eq!(
            ssh_host("root@203.0.113.7").unwrap(),
            ("root", "203.0.113.7")
        );
        for bad in ["host", "root@", "-oProxyCommand=x@h", "a@b;c", "a@-b"] {
            assert!(ssh_host(bad).is_err(), "{bad}");
        }
        check_cidr("203.0.113.7").unwrap();
        check_cidr("203.0.113.0/24").unwrap();
        assert!(check_cidr("0.0.0.0/0; rm").is_err());
    }

    #[test]
    fn the_script_checks_the_binary_and_firewalls_when_asked() {
        let leaf = Leaf {
            cert: "CERT\n".into(),
            key: "KEY\n".into(),
        };
        let s = render_script(
            "/tmp/isb-agent.x",
            "abc",
            "CA\n",
            &leaf,
            7443,
            &["198.51.100.1".into()],
            Some(22),
            true,
        );
        assert!(s.contains("echo \"abc  /tmp/isb-agent.x\" | sha256sum -c"));
        assert!(s.contains("ufw allow proto tcp from '198.51.100.1' to any port 7443"));
        assert!(s.contains("ufw allow 22/tcp"));
        assert!(s.contains("pkgs.zabbly.com/incus/stable"));
        assert!(s.contains("--agent-listen 0.0.0.0:7443"));
        assert!(s.contains("--ingress-https 0.0.0.0:443") && s.contains("--public-ingress"));
        let open = render_script(
            "/tmp/isb-agent.x",
            "abc",
            "CA\n",
            &leaf,
            7443,
            &[],
            Some(22),
            false,
        );
        assert!(!open.contains("ufw") && !open.contains("ingress"));
        let vm = render_script(
            "/tmp/isb-agent.x",
            "abc",
            "CA\n",
            &leaf,
            7443,
            &["10.0.3.1".into()],
            None,
            false,
        );
        assert!(
            vm.contains("ufw allow proto tcp from '10.0.3.1' to any port 7443")
                && !vm.contains("/tcp\nufw allow proto"),
            "a dedicated VM opens the agent port to the host only, and no SSH"
        );
        assert!(!vm.contains("ufw allow 22"));
        assert_eq!(
            elf_arch(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0\x3e\0"),
            Some("x86_64")
        );
        assert_eq!(elf_arch(b"#!/bin/sh"), None);
    }
}
