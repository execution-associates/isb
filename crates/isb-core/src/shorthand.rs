//! Command-line shorthands for mounts, ports, labels and readiness checks.
//!
//! These are also the compose file's short syntaxes.
//!
//! - Volume: `SRC:GUEST[:opt,...]`. `SRC` starting with `/`, `.` or `~` is a
//!   host path (bind); anything else is a named volume. Options: `ro`, `rw`,
//!   `owner=USER`, `device=NAME`, `pool=POOL`, `external`, plus docker's
//!   propagation modes and `z`/`Z` (ignored).
//! - Port (host listens): `[IP:]PUBLISHED:TARGET[/udp]`, as docker writes it
//!   but with IP defaulting to 127.0.0.1. `PUBLISHED` may be a range. Or the
//!   full form `listen=tcp:..,connect=tcp:..[,bind=guest][,name=N][,search=N]`.
//! - Ready: `running`, `default_route`, `user_exists=USER`, `path_writable=PATH`,
//!   `command=ARG[,ARG...]`.

use crate::error::{Error, Result};
use crate::spec::{PortBind, PortSpec, ReadyCheck, VolumeSpec, is_host_path};

/// `KEY=VALUE`.
pub fn key_value(s: &str) -> Result<(String, String)> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| Error::invalid(format!("expected KEY=VALUE, got {s:?}")))
}

/// Parse a `-v` value, or a compose short-syntax mount.
pub fn volume(s: &str) -> Result<VolumeSpec> {
    let mut parts = s.splitn(3, ':');
    let (Some(src), Some(guest)) = (parts.next(), parts.next()) else {
        return Err(Error::invalid(format!(
            "volume {s:?}: expected SOURCE:TARGET[:options] (isb has no anonymous volumes)"
        )));
    };
    if src.is_empty() || !guest.starts_with('/') {
        return Err(Error::invalid(format!(
            "volume {s:?}: expected SOURCE:TARGET with an absolute target"
        )));
    }
    let mut v = if is_host_path(src) {
        crate::spec::Volume::bind(src)
    } else {
        crate::spec::Volume::named(src)
    };
    v.target = guest.to_string();
    if let Some(opts) = parts.next() {
        for o in opts.split(',').filter(|o| !o.is_empty()) {
            match o.split_once('=') {
                None if o == "ro" => v.read_only = true,
                None if o == "rw" => v.read_only = false,
                // SELinux relabelling: nothing to do under incus.
                None if o == "z" || o == "Z" => {}
                None if matches!(
                    o,
                    "shared" | "rshared" | "slave" | "rslave" | "private" | "rprivate"
                ) =>
                {
                    v.options.insert("propagation".into(), o.into());
                }
                None if o == "external" => v.external = true,
                None if o == "nocopy" => v.volume.nocopy = true,
                Some(("owner", u)) => v.owner = Some(u.into()),
                Some(("device", d)) => v.device = Some(d.into()),
                Some(("pool", p)) => v.pool = Some(p.into()),
                _ => {
                    return Err(Error::invalid(format!(
                        "volume {s:?}: unknown option {o:?} (ro, rw, nocopy, owner=, device=, pool=, external)"
                    )));
                }
            }
        }
    }
    Ok(v)
}

/// A port or a `START-END` range.
fn port_range(s: &str, what: &str, spec: &str) -> Result<(u16, u16)> {
    let bad = || {
        Error::invalid(format!(
            "port {spec:?}: {what} {s:?} is not a port or range"
        ))
    };
    let parse = |n: &str| {
        n.trim()
            .parse::<u16>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(bad)
    };
    match s.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (parse(a)?, parse(b)?);
            if b < a {
                return Err(bad());
            }
            Ok((a, b))
        }
        None => parse(s).map(|n| (n, n)),
    }
}

/// A docker-style published port: listen on `host_ip` (default 127.0.0.1) at
/// `published`, connect to `target` in the guest. A published range with a
/// single target takes the first free port in the range; two ranges of the
/// same length map port to port.
pub fn docker_port(
    host_ip: Option<&str>,
    published: &str,
    target: &str,
    proto: &str,
) -> Result<PortSpec> {
    let spec = format!(
        "{}:{published}:{target}/{proto}",
        host_ip.unwrap_or("127.0.0.1")
    );
    if !matches!(proto, "tcp" | "udp") {
        return Err(Error::invalid(format!(
            "port {spec:?}: protocol is tcp or udp"
        )));
    }
    let host = match host_ip.map(str::trim).filter(|h| !h.is_empty()) {
        None => "127.0.0.1".to_string(),
        Some(h) if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
        Some(h) => h.to_string(),
    };
    let (pa, pb) = port_range(published, "published", &spec)?;
    let (ta, tb) = port_range(target, "target", &spec)?;
    let mut p = PortSpec {
        bind: PortBind::Host,
        ..Default::default()
    };
    if ta == tb {
        p.listen = format!("{proto}:{host}:{pa}");
        p.connect = format!("{proto}:{ta}");
        p.search = (pb > pa).then(|| pb - pa);
    } else if pb - pa == tb - ta {
        p.listen = format!("{proto}:{host}:{pa}-{pb}");
        p.connect = format!("{proto}:{ta}-{tb}");
    } else {
        return Err(Error::invalid(format!(
            "port {spec:?}: a target range needs a published range of the same length"
        )));
    }
    Ok(p)
}

/// Parse a `-p` value: the compose short syntax, or the CLI's
/// `listen=..,connect=..` form.
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
    docker_short_port(s)
}

/// Docker's short port syntax: `[HOST_IP:]PUBLISHED:TARGET[/PROTOCOL]`.
pub fn docker_short_port(s: &str) -> Result<PortSpec> {
    let (body, proto) = match s.rsplit_once('/') {
        Some((b, p)) if p == "tcp" || p == "udp" => (b, p),
        Some(_) => {
            return Err(Error::invalid(format!(
                "port {s:?}: protocol is tcp or udp"
            )));
        }
        None => (s, "tcp"),
    };
    // An IPv6 host is bracketed: [::1]:8080:80.
    let (ip, rest) = match body.strip_prefix('[') {
        Some(b) => {
            let (ip, rest) = b.split_once("]:").ok_or_else(|| {
                Error::invalid(format!("port {s:?}: expected [IPv6]:PUBLISHED:TARGET"))
            })?;
            (Some(ip), rest)
        }
        None => (None, body),
    };
    let parts: Vec<&str> = rest.rsplitn(3, ':').collect();
    let (ip, published, target) = match (ip, parts.as_slice()) {
        (None, [t, p]) => (None, *p, *t),
        (None, [t, p, ip]) => (Some(*ip), *p, *t),
        (Some(ip), [t, p]) => (Some(ip), *p, *t),
        (None, [_]) => {
            return Err(Error::invalid(format!(
                "port {s:?}: isb needs the host port too, e.g. {body}:{body} (docker would pick a random one)"
            )));
        }
        _ => {
            return Err(Error::invalid(format!(
                "port {s:?}: expected [HOST_IP:]PUBLISHED:TARGET[/udp]"
            )));
        }
    };
    docker_port(ip, published, target, proto)
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
        use crate::spec::MountType;
        let v = volume("./src:/app:ro").unwrap();
        assert_eq!(v.target, "/app");
        assert_eq!(
            (v.mount_type, v.source.as_str()),
            (MountType::Bind, "./src")
        );
        assert!(v.read_only);
        let v = volume("cache:/home/dev/.cache:owner=dev,device=c").unwrap();
        assert_eq!(v.target, "/home/dev/.cache");
        assert_eq!(
            (v.mount_type, v.source.as_str()),
            (MountType::Volume, "cache")
        );
        assert_eq!(v.owner.as_deref(), Some("dev"));
        assert_eq!(v.device.as_deref(), Some("c"));
        let v = volume("/srv:/srv:z,rslave").unwrap();
        assert_eq!(v.options["propagation"], "rslave");
        assert!(volume("/data").is_err());
        assert!(volume("x").is_err());
        assert!(volume("a:rel").is_err());
        assert!(volume("a:/b:bogus").is_err());
    }

    #[test]
    fn ports() {
        let p = port("8080:80").unwrap();
        assert_eq!(p.listen, "tcp:127.0.0.1:8080");
        assert_eq!(p.connect, "tcp:80");
        let p = port("100.1.2.3:5173:5173/udp").unwrap();
        assert_eq!(p.listen, "udp:100.1.2.3:5173");
        let p = port("100.1.2.3:5173-5223:5173").unwrap();
        assert_eq!(p.listen, "tcp:100.1.2.3:5173");
        assert_eq!(p.search, Some(50));
        let p = port("8000-8002:9000-9002").unwrap();
        assert_eq!(p.listen, "tcp:127.0.0.1:8000-8002");
        assert_eq!(p.connect, "tcp:9000-9002");
        assert_eq!(p.search, None);
        let p = port("[::1]:8080:80").unwrap();
        assert_eq!(p.listen, "tcp:[::1]:8080");
        assert!(port("8000:9000-9002").is_err());
        assert!(port("8000-8001:9000-9002").is_err());
        assert!(docker_short_port("listen=tcp:1.2.3.4:1,connect=tcp:1.2.3.4:2").is_err());
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
