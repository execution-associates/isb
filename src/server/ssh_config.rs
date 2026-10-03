//! What `isb ssh-config` writes: `Host` blocks whose `ProxyCommand` is
//! `isb ssh-proxy`, and a known_hosts file of the instances' host keys
//! under each block's `HostKeyAlias`, so plain `ssh`, `scp`, editors and
//! `herdr machine add` pin the right key without ever trusting on first use.

use std::path::Path;

/// One instance's block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub org: String,
    pub instance: String,
    /// The guest user to log in as.
    pub user: String,
    /// `algorithm base64` host keys; empty when the instance has none yet.
    pub host_keys: Vec<String>,
}

impl HostEntry {
    /// `<instance>.<org>.isb`: the `Host` name, and the key alias.
    pub fn alias(&self) -> String {
        alias(&self.org, &self.instance)
    }
}

pub fn alias(org: &str, instance: &str) -> String {
    format!("{instance}.{org}.isb")
}

/// How the blocks reach `isb ssh-proxy`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyOptions {
    /// The isb binary.
    pub isb: String,
    /// `--url` for a remote daemon; none for the local socket.
    pub url: Option<String>,
    /// `--token-file`, when the token is in a file rather than `ISB_TOKEN`.
    pub token_file: Option<String>,
    /// `--as`: whose keys, for a caller with no isb account (the socket).
    pub keys_of: Option<String>,
    /// `IdentityFile` (and `IdentitiesOnly yes`).
    pub identity: Option<String>,
    pub known_hosts: String,
}

/// A word for ssh_config: quoted when it has spaces or quotes.
fn word(s: &str) -> String {
    if !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#'))
    {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// The `Host` block for one instance.
pub fn render(e: &HostEntry, o: &ProxyOptions) -> String {
    let a = e.alias();
    let mut proxy = format!("{} ssh-proxy {}/{}", word(&o.isb), e.org, e.instance);
    if let Some(u) = &o.url {
        proxy.push_str(&format!(" --url {}", word(u)));
    }
    if let Some(t) = &o.token_file {
        proxy.push_str(&format!(" --token-file {}", word(t)));
    }
    if let Some(k) = &o.keys_of {
        proxy.push_str(&format!(" --as {}", word(k)));
    }
    let mut s = format!(
        "# {org}/{inst}: isb ssh-proxy over isb serve's websocket (isb ssh-config)\n\
         Host {a}\n\
         \x20 HostName {a}\n\
         \x20 User {user}\n\
         \x20 ProxyCommand {proxy}\n\
         \x20 HostKeyAlias {a}\n\
         \x20 UserKnownHostsFile {kh}\n\
         \x20 StrictHostKeyChecking {strict}\n\
         \x20 ServerAliveInterval 30\n\
         \x20 ServerAliveCountMax 4\n",
        org = e.org,
        inst = e.instance,
        user = word(&e.user),
        kh = word(&o.known_hosts),
        strict = if e.host_keys.is_empty() {
            "accept-new"
        } else {
            "yes"
        },
    );
    if let Some(i) = &o.identity {
        s.push_str(&format!(
            "  IdentityFile {}\n  IdentitiesOnly yes\n",
            word(i)
        ));
    }
    s
}

/// The `herdr machine add` line for an entry.
pub fn herdr_line(e: &HostEntry) -> String {
    format!(
        "herdr machine add {} --label {}/{}",
        e.alias(),
        e.org,
        e.instance
    )
}

/// `existing` known_hosts with every line for these entries' aliases
/// replaced by their current keys. Other lines are kept as they were.
pub fn update_known_hosts(existing: &str, entries: &[HostEntry]) -> String {
    let aliases: Vec<String> = entries.iter().map(HostEntry::alias).collect();
    let mut out: Vec<String> = existing
        .lines()
        .filter(|l| {
            let host = l.split_whitespace().next().unwrap_or("");
            !host.split(',').any(|h| aliases.iter().any(|a| a == h))
        })
        .map(String::from)
        .collect();
    for e in entries {
        for k in &e.host_keys {
            out.push(format!("{} {k}", e.alias()));
        }
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Rewrite `path` with [`update_known_hosts`], creating it (and its
/// directory) as needed. Written whole, then renamed into place.
pub fn write_known_hosts(path: &Path, entries: &[HostEntry]) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("isb-tmp");
    std::fs::write(&tmp, update_known_hosts(&existing, entries))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(keys: &[&str]) -> HostEntry {
        HostEntry {
            org: "acme".into(),
            instance: "box".into(),
            user: "dev".into(),
            host_keys: keys.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn renders_a_pinned_host_block() {
        let o = ProxyOptions {
            isb: "/usr/local/bin/isb".into(),
            url: Some("https://isb.example.com".into()),
            token_file: Some("/home/me/.config/isb/my token".into()),
            keys_of: None,
            identity: None,
            known_hosts: "/home/me/.config/isb/known_hosts".into(),
        };
        let s = render(&entry(&["ssh-ed25519 AAAA"]), &o);
        assert_eq!(
            s,
            "# acme/box: isb ssh-proxy over isb serve's websocket (isb ssh-config)\n\
             Host box.acme.isb\n\
             \x20 HostName box.acme.isb\n\
             \x20 User dev\n\
             \x20 ProxyCommand /usr/local/bin/isb ssh-proxy acme/box --url https://isb.example.com --token-file \"/home/me/.config/isb/my token\"\n\
             \x20 HostKeyAlias box.acme.isb\n\
             \x20 UserKnownHostsFile /home/me/.config/isb/known_hosts\n\
             \x20 StrictHostKeyChecking yes\n\
             \x20 ServerAliveInterval 30\n\
             \x20 ServerAliveCountMax 4\n"
        );
        assert_eq!(
            herdr_line(&entry(&[])),
            "herdr machine add box.acme.isb --label acme/box"
        );
    }

    #[test]
    fn no_host_key_yet_accepts_the_first_and_identity_is_pinned() {
        let o = ProxyOptions {
            isb: "isb".into(),
            keys_of: Some("me@example.com".into()),
            identity: Some("/tmp/k".into()),
            known_hosts: "/kh".into(),
            ..Default::default()
        };
        let s = render(&entry(&[]), &o);
        assert!(s.contains("StrictHostKeyChecking accept-new\n"), "{s}");
        assert!(s.contains("ProxyCommand isb ssh-proxy acme/box --as me@example.com\n"));
        assert!(s.contains("  IdentityFile /tmp/k\n  IdentitiesOnly yes\n"));
    }

    #[test]
    fn known_hosts_replaces_only_its_own_lines() {
        let old = "github.com ssh-ed25519 GH\nbox.acme.isb ssh-ed25519 OLD\nother.acme.isb ssh-rsa KEEP\n";
        let new = update_known_hosts(old, &[entry(&["ssh-ed25519 NEW", "ssh-rsa NEW2"])]);
        assert_eq!(
            new,
            "github.com ssh-ed25519 GH\nother.acme.isb ssh-rsa KEEP\nbox.acme.isb ssh-ed25519 NEW\nbox.acme.isb ssh-rsa NEW2\n"
        );
        // Idempotent.
        assert_eq!(
            update_known_hosts(&new, &[entry(&["ssh-ed25519 NEW", "ssh-rsa NEW2"])]),
            new
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("isb/known_hosts");
        write_known_hosts(&p, &[entry(&["ssh-ed25519 K"])]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "box.acme.isb ssh-ed25519 K\n"
        );
    }
}
