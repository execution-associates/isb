//! `isb host ...`: preparing the host (firewall, sysctl, DNS) for orgs and ingress.

use super::*;

#[derive(Subcommand)]
pub(crate) enum HostCmd {
    /// Let org bridges through a default-deny host firewall (ufw): DHCP and
    /// DNS to the host, and egress through the uplink. Also makes the
    /// directory service names are published in. Run once, as root.
    Setup {
        /// The uplink interface (default: the default route's).
        #[arg(long)]
        uplink: Option<String>,
        /// The user `isb serve` and `isb org` run as, who writes service
        /// names (default: the user who ran sudo).
        #[arg(long)]
        user: Option<String>,
        /// Print the firewall commands instead of running them.
        #[arg(long)]
        dry_run: bool,
        /// Also prepare a public ingress: open 80 and 443 in ufw, and let
        /// unprivileged users bind them (net.ipv4.ip_unprivileged_port_start=80),
        /// so `isb serve --ingress-http :80 --ingress-https :443` runs as you.
        #[arg(long)]
        public_ingress: bool,
    },
}

/// The ufw rules org bridges need on a default-deny host, as argv lists.
/// DHCP is not among them: see [`BEFORE_RULES`].
pub(crate) fn host_rules(uplink: &str, public_ingress: bool) -> Vec<Vec<String>> {
    let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let mut out = vec![
        v("ufw allow in on isbbr+ to any port 53 comment"),
        v(&format!(
            "ufw route allow in on isbbr+ out on {uplink} comment"
        )),
        // A Cloudflare-tunnel org's cloudflared reaches the ingress on its
        // own bridge address; other orgs' ACLs keep them off it.
        v(&format!(
            "ufw allow in on isbbr+ to any port {} proto tcp comment",
            isb::ingress::DEFAULT_TUNNEL_PORT
        )),
    ];
    let mut comments = vec![
        "isb org bridges: DNS",
        "isb org bridges: egress",
        "isb org bridges: tunnel ingress",
    ];
    if public_ingress {
        out.push(v("ufw allow 80/tcp comment"));
        out.push(v("ufw allow 443/tcp comment"));
        comments.extend(["isb ingress: http", "isb ingress: https"]);
    }
    for (r, c) in out.iter_mut().zip(comments) {
        r.push(c.to_string());
    }
    out
}

/// Lets the daemon's user bind 80 and 443 without root or capabilities.
pub(crate) const SYSCTL_PATH: &str = "/etc/sysctl.d/60-isb-ingress.conf";
pub(crate) const SYSCTL_TEXT: &str = "# isb serve's ingress binds 80 and 443 as an ordinary user.\nnet.ipv4.ip_unprivileged_port_start = 80\n";

/// Rules ufw's own commands cannot express, ahead of its defaults.
///
/// - DHCP, ahead of ufw's conntrack-INVALID drop. With br_netfilter on, the
///   bridged copy of a DHCP broadcast is dropped in FORWARD, and once the
///   bridge carries an incus ACL the copy meant for dnsmasq then counts as
///   INVALID; a `ufw allow` rule comes too late to see it.
/// - Traffic between instances of one org. With br_netfilter on, frames
///   bridged within an org's bridge traverse FORWARD, where ufw's routed
///   default-deny drops them (only ICMP got through). `--physdev-is-bridged`
///   matches only traffic that stays on one bridge; traffic between two org
///   bridges is routed, so it stays denied (and the org ACLs reject it too).
pub(crate) const BEFORE_RULES: &str = "# isb org bridges: begin\n\
-A ufw-before-input -i isbbr+ -p udp --dport 67 -j ACCEPT\n\
-A ufw-before-forward -i isbbr+ -o isbbr+ -m physdev --physdev-is-bridged -j ACCEPT\n\
# isb org bridges: end\n";

pub(crate) const BEFORE_RULES_PATH: &str = "/etc/ufw/before.rules";

/// `before.rules` with isb's block inserted before the first
/// `ufw-before-input` rule (or an older block replaced), or `None` when the
/// current block is already there.
pub(crate) fn with_before_rules(text: &str) -> Option<String> {
    if text.contains(BEFORE_RULES) {
        return None;
    }
    const END: &str = "# isb org bridges: end\n";
    if let (Some(a), Some(b)) = (text.find("# isb org bridges: begin"), text.find(END)) {
        if a < b {
            return Some(format!(
                "{}{BEFORE_RULES}{}",
                &text[..a],
                &text[b + END.len()..]
            ));
        }
    }
    let mut out = String::with_capacity(text.len() + BEFORE_RULES.len());
    let mut done = false;
    for line in text.split_inclusive('\n') {
        if !done && line.starts_with("-A ufw-before-input") {
            out.push_str(BEFORE_RULES);
            done = true;
        }
        out.push_str(line);
    }
    done.then_some(out)
}

/// The command that makes the service-name directory: owned by `user`,
/// group `incus` (dnsmasq's) with setgid so what the daemon writes there is
/// readable by dnsmasq and nobody else. Without an `incus` group (dnsmasq as
/// `nobody`), world-readable instead.
pub(crate) fn dns_dir_command(user: &str, incus_group: bool) -> Vec<String> {
    let root = isb::discovery::root().display().to_string();
    let (mode, group) = if incus_group {
        ("2750", isb::discovery::DNSMASQ_GROUP.to_string())
    } else {
        ("0755", user.to_string())
    };
    ["install", "-d", "-m", mode, "-o", user, "-g", &group, &root]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub(crate) fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group")
        .map(|t| t.lines().any(|l| l.split(':').next() == Some(name)))
        .unwrap_or(false)
}

#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn host_setup(
    uplink: Option<String>,
    user: Option<String>,
    dry_run: bool,
    public_ingress: bool,
) -> Result<u8> {
    let uplink = match uplink {
        Some(u) => u,
        None => default_route_iface()
            .ok_or_else(|| Error::Invalid("no default route; pass --uplink".into()))?,
    };
    let user = user
        .or_else(|| std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty()))
        .or_else(|| std::env::var("USER").ok().filter(|u| !u.is_empty()))
        .ok_or_else(|| Error::Invalid("cannot tell who runs isb; pass --user".into()))?;
    let dns_cmd = dns_dir_command(&user, group_exists(isb::discovery::DNSMASQ_GROUP));
    let ufw_active = std::process::Command::new("ufw")
        .arg("status")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("Status: active"));
    let rules = host_rules(&uplink, public_ingress);
    // The local registry's CA, where the skopeo inside incusd looks for it.
    let registry = isb::registry::info(&Client::new()).unwrap_or_else(|e| {
        eprintln!("isb: cannot read the local registry's settings: {e}");
        None
    });
    let ca_path = registry
        .as_ref()
        .map(|i| (isb::registry::host_ca_path(&i.addr), i.ca_pem.clone()));
    let ca_current = ca_path
        .as_ref()
        .is_some_and(|(p, pem)| std::fs::read_to_string(p).ok().as_deref() == Some(pem.as_str()));
    if dry_run || !rustix::process::geteuid().is_root() {
        if !dry_run {
            eprintln!(
                "isb host setup needs root to change the firewall; run it with sudo, or do this:"
            );
        }
        println!("# the directory service names are published in:");
        println!("{}", dns_cmd.join(" "));
        match &ca_path {
            Some((p, _)) if ca_current => {
                println!("# the local registry's CA is installed at {}", p.display());
            }
            Some((p, _)) => {
                println!(
                    "# trust the local registry (its CA, from incus project {}):",
                    isb::registry::PROJECT
                );
                println!(
                    "install -d -m 0755 {}",
                    p.parent().expect("has a parent").display()
                );
                println!(
                    "incus project get {} user.isb.registry.ca > {} && chmod 0644 {}",
                    isb::registry::PROJECT,
                    p.display(),
                    p.display()
                );
            }
            None => println!("# no local registry yet (isb registry setup); its CA comes later"),
        }
        println!("# in {BEFORE_RULES_PATH}, before the first -A ufw-before-input line:");
        print!("{BEFORE_RULES}");
        for r in &rules {
            println!(
                "{}",
                r.iter()
                    .map(|a| if a.contains(' ') {
                        format!("'{a}'")
                    } else {
                        a.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        println!("ufw reload");
        if public_ingress {
            println!("# {SYSCTL_PATH}:");
            print!("{SYSCTL_TEXT}");
            println!("sysctl -p {SYSCTL_PATH}");
        }
        return Ok(if dry_run { 0 } else { 1 });
    }
    if public_ingress {
        std::fs::write(SYSCTL_PATH, SYSCTL_TEXT)?;
        if !std::process::Command::new("sysctl")
            .args(["-p", SYSCTL_PATH])
            .stdout(std::process::Stdio::null())
            .status()?
            .success()
        {
            return Err(Error::Invalid(format!("sysctl -p {SYSCTL_PATH} failed")));
        }
        println!("{SYSCTL_PATH}: ordinary users may bind ports 80 and up");
    }
    if !std::process::Command::new(&dns_cmd[0])
        .args(&dns_cmd[1..])
        .status()?
        .success()
    {
        return Err(Error::Invalid(format!("{} failed", dns_cmd.join(" "))));
    }
    println!(
        "{}: service names for org stacks, written by {user}",
        isb::discovery::root().display()
    );
    if let Some((p, pem)) = &ca_path {
        if !ca_current {
            use std::os::unix::fs::PermissionsExt;
            let dir = p.parent().expect("has a parent");
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
            std::fs::write(p, pem)?;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644))?;
            println!(
                "{}: the local registry's CA (incus pulls trust it)",
                p.display()
            );
        }
    }
    if !ufw_active {
        println!("no active ufw: incus' own firewall rules already let org bridges through");
        return Ok(0);
    }
    let text = std::fs::read_to_string(BEFORE_RULES_PATH)?;
    if let Some(new) = with_before_rules(&text) {
        std::fs::write(format!("{BEFORE_RULES_PATH}.isb-backup"), &text)?;
        std::fs::write(BEFORE_RULES_PATH, new)?;
        println!(
            "{BEFORE_RULES_PATH}: wrote isb's DHCP and same-org rules (backup at {BEFORE_RULES_PATH}.isb-backup)"
        );
    }
    for r in rules {
        let st = std::process::Command::new(&r[0]).args(&r[1..]).status()?;
        if !st.success() {
            return Err(Error::Invalid(format!("{} failed", r.join(" "))));
        }
    }
    if !std::process::Command::new("ufw")
        .arg("reload")
        .status()?
        .success()
    {
        return Err(Error::Invalid("ufw reload failed".into()));
    }
    println!(
        "org bridges (isbbr*) may now reach DHCP and DNS on this host and egress through {uplink}"
    );
    Ok(0)
}

pub(crate) fn default_route_iface() -> Option<String> {
    let t = std::fs::read_to_string("/proc/net/route").ok()?;
    t.lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.get(1) == Some(&"00000000"))
        .map(|f| f[0].to_string())
}
