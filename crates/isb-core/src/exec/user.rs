//! Resolving an exec `user` (`dev`, `1000`, `1000:1000`, `dev:staff`) to ids
//! inside the guest.

use super::*;

/// What user resolution needs from a guest: its files (through the file
/// API, no program runs) and `getent` (a program, which minimal images lack).
trait Guest {
    fn read_file(&self, path: &str) -> Result<Option<String>>;
    /// `getent DB KEY`: the first line when found; `None` when `getent`
    /// is missing, fails to run, or does not know the key.
    fn getent(&self, db: &str, key: &str) -> Option<String>;
}

struct Instance<'a> {
    client: &'a Client,
    name: &'a str,
}

impl Guest for Instance<'_> {
    fn read_file(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .client
            .read_file(self.name, path)?
            .map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    fn getent(&self, db: &str, key: &str) -> Option<String> {
        let out = run_captured(
            self.client,
            self.name,
            &["getent".into(), db.into(), key.into()],
            &Request::root(),
            Stdin::Null,
            Some(Duration::from_secs(30)),
        )
        .ok()?;
        if !out.success() {
            return None;
        }
        out.stdout_text().lines().next().map(str::to_string)
    }
}

/// Resolve `dev`, `1000`, `1000:1000` or `dev:staff` to ids inside the
/// guest. Numeric ids need no lookup (the home and shell are read from
/// `/etc/passwd` when it is there); names go through `getent` and, where
/// the image has none, through `/etc/passwd` and `/etc/group`.
#[doc(hidden)]
pub fn resolve_user(client: &Client, instance: &str, user: &str) -> Result<GuestUser> {
    resolve_in(
        &Instance {
            client,
            name: instance,
        },
        instance,
        user,
    )
}

fn resolve_in(g: &dyn Guest, instance: &str, user: &str) -> Result<GuestUser> {
    let (u, group) = match user.split_once(':') {
        Some((u, grp)) => (u, Some(grp)),
        None => (user, None),
    };
    let numeric = u.parse::<u32>().ok();
    // A numeric uid is never looked up with a program: only the file.
    let entry = match numeric {
        Some(_) => file_entry(g, u),
        None => passwd_entry(g, u),
    };
    let mut gu = entry.unwrap_or_default();
    let found = gu.name.is_some();
    match numeric {
        Some(uid) => {
            gu.uid = uid;
            if !found {
                gu.gid = uid;
            }
        }
        None if !found => return Err(no_such("user", u, instance)),
        None => {}
    }
    if let Some(grp) = group {
        gu.gid = match grp.parse::<u32>() {
            Ok(n) => n,
            Err(_) => group_id(g, grp).ok_or_else(|| no_such("group", grp, instance))?,
        };
    }
    Ok(gu)
}

fn no_such(what: &str, name: &str, instance: &str) -> Error {
    Error::invalid(format!("no such {what} {name} in {instance}"))
}

/// The passwd entry for a user name: `getent` first, then the file.
fn passwd_entry(g: &dyn Guest, name: &str) -> Option<GuestUser> {
    g.getent("passwd", name)
        .as_deref()
        .and_then(parse_passwd)
        .or_else(|| file_entry(g, name))
}

/// The entry for a name or numeric uid in `/etc/passwd`; a file that
/// cannot be read is no entry.
fn file_entry(g: &dyn Guest, key: &str) -> Option<GuestUser> {
    let text = g.read_file("/etc/passwd").ok()??;
    let uid = key.parse::<u32>().ok();
    text.lines().filter_map(parse_passwd).find(|u| match uid {
        Some(n) => u.uid == n,
        None => u.name.as_deref() == Some(key),
    })
}

fn group_id(g: &dyn Guest, name: &str) -> Option<u32> {
    let from_line = |l: &str| l.split(':').nth(2).and_then(|n| n.parse().ok());
    if let Some(n) = g.getent("group", name).as_deref().and_then(from_line) {
        return Some(n);
    }
    let text = g.read_file("/etc/group").ok()??;
    text.lines()
        .find(|l| l.split(':').next() == Some(name))
        .and_then(from_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    /// A guest with files and, optionally, a working `getent` over them.
    struct Fake {
        files: HashMap<&'static str, &'static str>,
        getent: HashMap<(&'static str, &'static str), &'static str>,
    }

    impl Guest for Fake {
        fn read_file(&self, path: &str) -> Result<Option<String>> {
            Ok(self.files.get(path).map(|s| s.to_string()))
        }
        fn getent(&self, db: &str, key: &str) -> Option<String> {
            self.getent.get(&(db, key)).map(|s| s.to_string())
        }
    }

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nstaff:x:50:\nnogroup:x:65534:\n";

    fn busybox() -> Fake {
        Fake {
            files: HashMap::from([("/etc/passwd", PASSWD), ("/etc/group", GROUP)]),
            getent: HashMap::new(),
        }
    }

    /// An image with no files and no getent (traefik/whoami).
    fn bare() -> Fake {
        Fake {
            files: HashMap::new(),
            getent: HashMap::new(),
        }
    }

    #[test]
    fn numeric_users_need_nothing_in_the_guest() {
        let u = resolve_in(&bare(), "i", "1000").unwrap();
        assert_eq!((u.uid, u.gid, u.name, u.home), (1000, 1000, None, None));
        let u = resolve_in(&bare(), "i", "1000:2000").unwrap();
        assert_eq!((u.uid, u.gid), (1000, 2000));
        let e = resolve_in(&bare(), "i", "7:staff").unwrap_err();
        assert!(e.to_string().contains("no such group staff in i"), "{e}");
    }

    #[test]
    fn numeric_users_pick_up_home_and_shell_when_passwd_is_readable() {
        let u = resolve_in(&busybox(), "i", "65534").unwrap();
        assert_eq!((u.uid, u.gid), (65534, 65534));
        assert_eq!(u.name.as_deref(), Some("nobody"));
        assert_eq!(u.home.as_deref(), Some("/nonexistent"));
        let u = resolve_in(&busybox(), "i", "65534:50").unwrap();
        assert_eq!((u.uid, u.gid), (65534, 50));
        // Not in the file: still fine.
        let u = resolve_in(&busybox(), "i", "1000").unwrap();
        assert_eq!((u.uid, u.gid, u.home), (1000, 1000, None));
    }

    #[test]
    fn names_fall_back_to_the_files_without_getent() {
        let u = resolve_in(&busybox(), "i", "nobody").unwrap();
        assert_eq!((u.uid, u.gid), (65534, 65534));
        assert_eq!(u.shell.as_deref(), Some("/sbin/nologin"));
        let u = resolve_in(&busybox(), "i", "nobody:staff").unwrap();
        assert_eq!((u.uid, u.gid), (65534, 50));
        let e = resolve_in(&busybox(), "web", "ghost").unwrap_err();
        assert!(e.to_string().contains("no such user ghost in web"), "{e}");
        let e = resolve_in(&busybox(), "web", "nobody:ghosts").unwrap_err();
        assert!(e.to_string().contains("no such group ghosts in web"), "{e}");
        // No files at all: a name cannot be resolved.
        assert!(resolve_in(&bare(), "web", "dev").is_err());
    }

    #[test]
    fn getent_answers_first() {
        let g = Fake {
            files: HashMap::from([("/etc/passwd", PASSWD)]),
            getent: HashMap::from([
                (("passwd", "dev"), "dev:x:1000:1000::/home/dev:/bin/bash"),
                (("group", "ops"), "ops:x:77:"),
            ]),
        };
        let u = resolve_in(&g, "i", "dev:ops").unwrap();
        assert_eq!((u.uid, u.gid), (1000, 77));
        assert_eq!(u.home.as_deref(), Some("/home/dev"));
        // getent does not know it, the file does.
        assert_eq!(resolve_in(&g, "i", "nobody").unwrap().uid, 65534);
    }
}
