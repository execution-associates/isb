//! A named volume's mount point: its owner and mode, set from the host.
//!
//! Users and groups resolve against the image's own `/etc/passwd` and
//! `/etc/group` (read through the file API); the stat, chown and chmod go
//! through SFTP in the instance's namespace. Nothing runs in the guest, so an
//! image with no shell (distroless, scratch) works the same as any other.

use std::time::Duration;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::sftp::Sftp;

/// The mode incus gives the root of a new, unseeded volume.
pub const FRESH_MODE: u32 = 0o711;

/// An octal mode such as `0770` (or `770`, `0o770`).
pub fn parse_mode(m: &str) -> std::result::Result<u32, String> {
    u32::from_str_radix(m.trim().trim_start_matches("0o"), 8)
        .ok()
        .filter(|v| *v <= 0o7777)
        .ok_or_else(|| format!("mode {m:?} is not an octal mode like 0770"))
}

/// `UID` or `UID:GID`, both numeric: ids known without the image.
pub fn numeric_ids(owner: &str) -> Option<(u32, u32)> {
    let (u, g) = owner.split_once(':').unwrap_or((owner, owner));
    Some((u.parse().ok()?, g.parse().ok()?))
}

/// What to do to one mount point.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fix<'a> {
    pub path: &'a str,
    /// `USER`, `USER:GROUP` or numeric ids.
    pub owner: Option<&'a str>,
    pub mode: Option<u32>,
    /// Only if the mount point is still what incus makes of a new volume
    /// (root's, mode 0711 or `mode`): the image did not seed it.
    pub fresh_only: bool,
}

/// Apply `fix` in instance `name`. Returns false when `fresh_only` left the
/// mount point alone.
pub fn apply(client: &Client, name: &str, fix: &Fix) -> Result<bool> {
    let step = |what: &str| format!("{what} {} in {name}", fix.path);
    let resolved = match fix.owner {
        Some(o) => Some(
            resolve(client, name, o).map_err(|e| Error::OperationFailed {
                step: step(&format!("chown {o}")),
                message: e,
            })?,
        ),
        None => None,
    };
    let mut sftp = Sftp::open(client, name, Duration::from_secs(30))?;
    let st = sftp.stat(fix.path)?.ok_or_else(|| Error::OperationFailed {
        step: step("stat"),
        message: "the mount point does not exist".into(),
    })?;
    if fix.fresh_only && (st.uid != 0 || (st.mode != FRESH_MODE && Some(st.mode) != fix.mode)) {
        return Ok(false);
    }
    if let Some(r) = &resolved {
        sftp.chown(fix.path, r.uid, r.gid)?;
        // Parents the mount conjured are root-owned; fix those inside the
        // user's home only, and stop at the first one that is not root's.
        if let Some(home) = r.home.as_deref().filter(|h| !h.is_empty() && *h != "/") {
            if fix
                .path
                .starts_with(&format!("{}/", home.trim_end_matches('/')))
            {
                let mut d = parent(fix.path);
                while d != home.trim_end_matches('/') && d != "/" {
                    match sftp.stat(&d)? {
                        Some(s) if s.uid == 0 => sftp.chown(&d, r.uid, r.gid)?,
                        _ => break,
                    }
                    d = parent(&d);
                }
            }
        }
    }
    if let Some(m) = fix.mode {
        sftp.chmod(fix.path, m)?;
    }
    Ok(true)
}

fn parent(p: &str) -> String {
    match p.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((d, _)) => d.into(),
    }
}

#[derive(Debug, PartialEq)]
struct Ids {
    uid: u32,
    gid: u32,
    home: Option<String>,
}

fn resolve(client: &Client, name: &str, owner: &str) -> std::result::Result<Ids, String> {
    let read = |path: &str| -> std::result::Result<String, String> {
        match client.read_file(name, path) {
            Ok(b) => Ok(String::from_utf8_lossy(&b.unwrap_or_default()).into_owned()),
            Err(e) => Err(format!("read {path}: {e}")),
        }
    };
    let (_, group) = split(owner);
    // Read even for numeric ids: the entry's home bounds the parents fixed.
    let passwd = read("/etc/passwd")?;
    let groups = match group {
        Some(g) if g.parse::<u32>().is_err() => read("/etc/group")?,
        _ => String::new(),
    };
    resolve_in(owner, &passwd, &groups)
}

fn split(owner: &str) -> (&str, Option<&str>) {
    match owner.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (owner, None),
    }
}

/// `getent`'s answer from the files' text: a user by name, or a number by
/// uid; a number with no entry is that uid with the same gid and no home.
fn resolve_in(owner: &str, passwd: &str, group: &str) -> std::result::Result<Ids, String> {
    let (user, grp) = split(owner);
    let entry = passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 6 && (f[0] == user || (user.parse::<u32>().is_ok() && f[2] == user)))
            .then_some(f)
    });
    let mut ids = match entry {
        Some(f) => Ids {
            uid: f[2]
                .parse()
                .map_err(|_| format!("bad uid for {user} in /etc/passwd"))?,
            gid: f[3]
                .parse()
                .map_err(|_| format!("bad gid for {user} in /etc/passwd"))?,
            home: Some(f[5].to_string()),
        },
        None => match user.parse() {
            Ok(n) => Ids {
                uid: n,
                gid: n,
                home: None,
            },
            Err(_) => return Err(format!("no such user in the image's /etc/passwd: {user}")),
        },
    };
    match grp {
        None | Some("") => {}
        Some(g) => {
            ids.gid = match g.parse() {
                Ok(n) => n,
                Err(_) => group
                    .lines()
                    .map(|l| l.split(':').collect::<Vec<_>>())
                    .find(|f| f.len() >= 3 && f[0] == g)
                    .and_then(|f| f[2].parse().ok())
                    .ok_or_else(|| format!("no such group in the image's /etc/group: {g}"))?,
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\ndev:x:1000:1000::/home/dev:/bin/bash\napp:x:999:998::/:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nstaff:x:50:dev\n";

    fn ids(uid: u32, gid: u32, home: Option<&str>) -> Ids {
        Ids {
            uid,
            gid,
            home: home.map(String::from),
        }
    }

    #[test]
    fn owners_resolve_as_getent_and_chown_do() {
        assert_eq!(
            resolve_in("dev", PASSWD, GROUP),
            Ok(ids(1000, 1000, Some("/home/dev")))
        );
        assert_eq!(
            resolve_in("1000", PASSWD, GROUP),
            Ok(ids(1000, 1000, Some("/home/dev")))
        );
        assert_eq!(
            resolve_in("dev:staff", PASSWD, GROUP),
            Ok(ids(1000, 50, Some("/home/dev")))
        );
        assert_eq!(resolve_in("app:7", PASSWD, ""), Ok(ids(999, 7, Some("/"))));
        // No passwd entry (or no /etc/passwd at all): the number, twice.
        assert_eq!(resolve_in("4242", PASSWD, ""), Ok(ids(4242, 4242, None)));
        assert_eq!(resolve_in("4242:7", "", ""), Ok(ids(4242, 7, None)));
        let e = resolve_in("nobody", PASSWD, GROUP).unwrap_err();
        assert!(e.contains("no such user") && e.contains("nobody"), "{e}");
        let e = resolve_in("dev:wheel", PASSWD, GROUP).unwrap_err();
        assert!(e.contains("no such group") && e.contains("wheel"), "{e}");
    }

    #[test]
    fn modes_and_numeric_ids() {
        assert_eq!(parse_mode("0770"), Ok(0o770));
        assert_eq!(parse_mode("2775"), Ok(0o2775));
        assert_eq!(parse_mode("0o750"), Ok(0o750));
        assert!(parse_mode("0890").is_err());
        assert!(parse_mode("17777").is_err());
        assert_eq!(numeric_ids("1000"), Some((1000, 1000)));
        assert_eq!(numeric_ids("1000:50"), Some((1000, 50)));
        assert_eq!(numeric_ids("dev"), None);
        assert_eq!(numeric_ids("1000:staff"), None);
    }

    #[test]
    fn parents() {
        assert_eq!(parent("/home/dev/.cache/x"), "/home/dev/.cache");
        assert_eq!(parent("/data"), "/");
        assert_eq!(parent("/data/"), "/");
    }
}
