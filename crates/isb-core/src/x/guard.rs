//! Which host directory a run mounts: the project root, never a directory
//! that holds the caller's credentials or the host itself.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The project a directory belongs to: the nearest ancestor (or itself)
/// holding `.git`, a directory or a worktree's file, short of `home`, its
/// ancestors and `/`; else the directory itself. Read from the filesystem, never by running git, whose
/// config in an untrusted repository can run commands.
pub fn project_root(cwd: &Path, home: &Path) -> PathBuf {
    let mut d = cwd;
    loop {
        if d.join(".git").symlink_metadata().is_ok() {
            return d.to_path_buf();
        }
        match d.parent() {
            Some(p) if p != Path::new("/") && !home.starts_with(p) => d = p,
            _ => return cwd.to_path_buf(),
        }
    }
}

/// Paths a sandbox must never see, nor any directory holding one of them,
/// each also as its symlink target: on macOS `/etc` is `/private/etc`, and
/// a root is compared after canonicalizing.
fn sensitive(home: &Path) -> Vec<PathBuf> {
    let in_home = [
        ".ssh",
        ".config",
        ".local",
        ".gnupg",
        ".claude",
        ".codex",
        ".docker",
        ".kube",
        ".aws",
        ".azure",
        ".op",
        ".password-store",
        ".netrc",
        ".git-credentials",
        ".npmrc",
        ".pypirc",
        ".cargo/credentials.toml",
    ];
    let host = [
        "/etc",
        "/root",
        "/run",
        "/var/run",
        "/var/lib/incus",
        "/var/snap",
        "/proc",
        "/sys",
        "/dev",
        "/boot",
    ];
    in_home
        .iter()
        .map(|p| home.join(p))
        .chain(host.iter().map(PathBuf::from))
        .flat_map(|p| {
            let real = p.canonicalize().ok().filter(|c| *c != p);
            std::iter::once(p).chain(real)
        })
        .collect()
}

/// `root` made canonical (symlinks followed), or why it may not be mounted:
/// `/`, the home directory or one of its ancestors, or anything that is,
/// is inside, or contains a credential directory or a host path.
pub fn check_root(root: &Path, home: &Path) -> Result<PathBuf> {
    let r = root
        .canonicalize()
        .map_err(|e| Error::invalid(format!("{}: {e}", root.display())))?;
    let h = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let refuse = |why: String| {
        Err(Error::invalid(format!(
            "refusing to mount {}: {why}; run it from a project directory, or name one with --root",
            r.display()
        )))
    };
    if r == Path::new("/") {
        return refuse("it is the whole filesystem".into());
    }
    if h.starts_with(&r) {
        return refuse(format!("it holds your home directory {}", h.display()));
    }
    for s in sensitive(&h) {
        if r.starts_with(&s) {
            return refuse(format!("it is inside {}", s.display()));
        }
        if s.starts_with(&r) {
            return refuse(format!("it contains {}", s.display()));
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_the_nearest_repository_below_home() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let repo = home.join("src/app");
        let deep = repo.join("pkg/web");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(project_root(&deep, &home), repo);
        // A worktree's .git is a file.
        let wt = home.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(project_root(&wt, &home), wt);
        // No repository: the directory itself, never home or above.
        let plain = home.join("notes/x");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(home.join(".git")).unwrap();
        assert_eq!(project_root(&plain, &home), plain);
    }

    #[test]
    fn home_its_ancestors_and_credential_dirs_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        for d in ["proj", ".ssh/keys", ".config/op", "proj2"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        assert!(check_root(&home.join("proj"), &home).is_ok());
        for bad in [home.clone(), t.path().to_path_buf(), PathBuf::from("/")] {
            let e = check_root(&bad, &home).unwrap_err().to_string();
            assert!(e.contains("refusing"), "{e}");
        }
        let e = check_root(&home.join(".ssh/keys"), &home)
            .unwrap_err()
            .to_string();
        assert!(e.contains("inside") && e.contains(".ssh"), "{e}");
        assert!(check_root(&home.join(".config/op"), &home).is_err());
        assert!(check_root(Path::new("/etc"), &home).is_err());
        assert!(check_root(Path::new("/var/lib"), &home).is_err());
        // A symlink into a credential directory is judged by its target.
        std::os::unix::fs::symlink(home.join(".ssh"), home.join("proj2/link")).unwrap();
        assert!(check_root(&home.join("proj2/link"), &home).is_err());
    }

    #[test]
    fn a_credential_dir_that_is_a_symlink_guards_its_target() {
        // As /etc -> /private/etc on macOS: the root is canonical, so the
        // sensitive path must be compared as its target too.
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let keys = t.path().join("keys/sub");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&keys).unwrap();
        std::os::unix::fs::symlink(t.path().join("keys"), home.join(".ssh")).unwrap();
        let e = check_root(&keys, &home).unwrap_err().to_string();
        assert!(e.contains("inside"), "{e}");
    }
}
