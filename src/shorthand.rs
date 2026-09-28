//! Command-line shorthands for mounts, ports, labels and readiness checks.
//!
//! - Volume: `SRC:GUEST[:opt,...]`. `SRC` starting with `/`, `.` or `~` is a
//!   host path (bind); anything else is a named volume. Options: `ro`,
//!   `owner=USER`, `device=NAME`, `pool=POOL`, `external`.
//! - Port (host listens): `[IP:]HOSTPORT:GUESTPORT[/udp]`, guest side
//!   on 127.0.0.1. Or the full form `listen=tcp:..,connect=tcp:..[,bind=guest]
//!   [,name=N][,search=N]`.
//! - Ready: `running`, `default_route`, `user_exists=USER`, `path_writable=PATH`,
//!   `command=ARG[,ARG...]`.

use crate::error::{Error, Result};
use crate::spec::{PortBind, PortSpec, ReadyCheck, VolumeSpec};

/// `KEY=VALUE`.
pub fn key_value(s: &str) -> Result<(String, String)> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| Error::invalid(format!("expected KEY=VALUE, got {s:?}")))
}

/// Parse a `-v` value into (guest path, volume).
pub fn volume(s: &str) -> Result<(String, VolumeSpec)> {
    let mut parts = s.splitn(3, ':');
    let (Some(src), Some(guest)) = (parts.next(), parts.next()) else {
        return Err(Error::invalid(format!(
            "volume {s:?}: expected SRC:GUEST_PATH[:options]"
        )));
    };
    if src.is_empty() || !guest.starts_with('/') {
        return Err(Error::invalid(format!(
            "volume {s:?}: expected SRC:GUEST_PATH with an absolute guest path"
        )));
    }
    let mut v = if src.starts_with('/') || src.starts_with('.') || src.starts_with('~') {
        crate::spec::Volume::bind(src)
    } else {
        crate::spec::Volume::named(src)
    };
    if let Some(opts) = parts.next() {
        for o in opts.split(',').filter(|o| !o.is_empty()) {
            match o.split_once('=') {
                None if o == "ro" => v.readonly = true,
                None if o == "rw" => v.readonly = false,
                None if o == "external" => v.external = true,
                Some(("owner", u)) => v.owner = Some(u.into()),
                Some(("device", d)) => v.device = Some(d.into()),
                Some(("pool", p)) => v.pool = Some(p.into()),
                _ => {
                    return Err(Error::invalid(format!(
                        "volume {s:?}: unknown option {o:?} (ro, rw, owner=, device=, pool=, external)"
                    )));
                }
            }
        }
    }
    Ok((guest.to_string(), v))
}

/// Parse a `-p` value.
pub fn port(s: &str) -> Result<PortSpec> {
    if s.contains('=') {
        let mut p = PortSpec::default();
        let mut have = (false, false);
        // Split on commas that start a new key (addresses contain no commas).
        for kv in s.split(',') {
            let (k, v) = key_value(kv)?;
            match k.as_str() {
                "listen" => {
                    p.listen = v;
                    have.0 = true;
                }
                "connect" => {
                    p.connect = v;
                    have.1 = true;
                }
                "bind" => {
                    p.bind = match v.as_str() {
                        "host" => PortBind::Host,
                        "guest" => PortBind::Guest,
                        _ => {
                            return Err(Error::invalid(format!(
                                "port {s:?}: bind is host or guest"
                            )));
                        }
                    }
                }
                "name" => p.name = Some(v),
                "search" => {
                    p.search = Some(v.parse().map_err(|_| {
                        Error::invalid(format!("port {s:?}: search must be a number"))
                    })?)
                }
                // incus proxy options; anything else is almost certainly a typo.
                "nat" | "proxy_protocol" | "security.uid" | "security.gid" | "uid" | "gid"
                | "mode" => {
                    p.options.insert(k, v);
                }
                other => {
                    return Err(Error::invalid(format!(
                        "port {s:?}: unknown key {other:?} (listen, connect, bind, name, search, nat, proxy_protocol, uid, gid, mode, security.uid, security.gid)"
                    )));
                }
            }
        }
        if !(have.0 && have.1) {
            return Err(Error::invalid(format!(
                "port {s:?}: needs listen= and connect="
            )));
        }
        return Ok(p);
    }
    let (body, proto) = match s.rsplit_once('/') {
        Some((b, p)) if p == "tcp" || p == "udp" => (b, p),
        Some(_) => {
            return Err(Error::invalid(format!(
                "port {s:?}: protocol is tcp or udp"
            )));
        }
        None => (s, "tcp"),
    };
    let parts: Vec<&str> = body.rsplitn(3, ':').collect();
    let (ip, host_port, guest_port) = match parts.as_slice() {
        [g, h] => ("127.0.0.1", *h, *g),
        [g, h, ip] => (*ip, *h, *g),
        _ => {
            return Err(Error::invalid(format!(
                "port {s:?}: expected [IP:]HOST_PORT:GUEST_PORT[/udp]"
            )));
        }
    };
    for n in [host_port, guest_port] {
        n.parse::<u16>()
            .map_err(|_| Error::invalid(format!("port {s:?}: {n:?} is not a port number")))?;
    }
    Ok(PortSpec {
        bind: PortBind::Host,
        listen: format!("{proto}:{ip}:{host_port}"),
        connect: format!("{proto}:127.0.0.1:{guest_port}"),
        ..Default::default()
    })
}

/// Parse a `--ready` value.
pub fn ready(s: &str) -> Result<ReadyCheck> {
    Ok(match s.split_once('=') {
        None if s == "running" => ReadyCheck::Running,
        None if s == "default_route" => ReadyCheck::DefaultRoute,
        None if s == "agent" => ReadyCheck::Agent,
        Some(("user_exists", u)) => ReadyCheck::UserExists(u.into()),
        Some(("path_writable", p)) => ReadyCheck::PathWritable(p.into()),
        Some(("command", c)) => ReadyCheck::Command(c.split(',').map(String::from).collect()),
        _ => {
            return Err(Error::invalid(format!(
                "ready {s:?}: running, agent, default_route, user_exists=USER, path_writable=PATH or command=ARG,ARG"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volumes() {
        let (g, v) = volume("./src:/app:ro").unwrap();
        assert_eq!(g, "/app");
        assert_eq!(v.bind.as_deref(), Some("./src"));
        assert!(v.readonly);
        let (g, v) = volume("cache:/home/dev/.cache:owner=dev,device=c").unwrap();
        assert_eq!(g, "/home/dev/.cache");
        assert_eq!(v.named.as_deref(), Some("cache"));
        assert_eq!(v.owner.as_deref(), Some("dev"));
        assert_eq!(v.device.as_deref(), Some("c"));
        assert!(volume("x").is_err());
        assert!(volume("a:rel").is_err());
        assert!(volume("a:/b:bogus").is_err());
    }

    #[test]
    fn ports() {
        let p = port("8080:80").unwrap();
        assert_eq!(p.listen, "tcp:127.0.0.1:8080");
        assert_eq!(p.connect, "tcp:127.0.0.1:80");
        let p = port("100.1.2.3:5173:5173/udp").unwrap();
        assert_eq!(p.listen, "udp:100.1.2.3:5173");
        let p =
            port("bind=guest,listen=tcp:127.0.0.1:8190,connect=tcp:127.0.0.1:9000,name=backend")
                .unwrap();
        assert_eq!(p.bind, PortBind::Guest);
        assert_eq!(p.name.as_deref(), Some("backend"));
        let p = port("listen=tcp:1.2.3.4:5173,connect=tcp:127.0.0.1:5173,search=50").unwrap();
        assert_eq!(p.search, Some(50));
        assert!(port("80").is_err());
        assert!(port("a:b").is_err());
        assert!(port("listen=tcp:1.2.3.4:1").is_err());
        assert!(port("1:2/sctp").is_err());
        assert!(port("listen=tcp:1.2.3.4:1,connect=tcp:1.2.3.4:2,serach=5").is_err());
    }

    #[test]
    fn readies() {
        assert_eq!(ready("running").unwrap(), ReadyCheck::Running);
        assert_eq!(
            ready("user_exists=dev").unwrap(),
            ReadyCheck::UserExists("dev".into())
        );
        assert_eq!(
            ready("command=test,-d,/tmp").unwrap(),
            ReadyCheck::Command(vec!["test".into(), "-d".into(), "/tmp".into()])
        );
        assert!(ready("nope").is_err());
    }
}
