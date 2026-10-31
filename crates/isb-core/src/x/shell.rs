//! `isb x update-shell`: put the shim directory first on PATH in the user's
//! shell startup files, as a marked block that is found again, left alone
//! when present, and taken out by `--remove`.
//!
//! The block is appended at the end of the file, because the shims must come
//! before version managers' directories and those are usually set up
//! earlier in the same file (`mise activate`, nvm, asdf). A line the user
//! wrote that already puts the directory on PATH counts as present.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The block's first line.
pub const BEGIN: &str = "# >>> isb x shims >>>";
/// The block's last line.
pub const END: &str = "# <<< isb x shims <<<";

/// A shell whose startup file isbx can edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub const ALL: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }

    /// The shell a program path or name (`$SHELL`, `/bin/zsh`) is.
    pub fn of(program: &str) -> Option<Shell> {
        let n = Path::new(program).file_name()?.to_str()?;
        Shell::ALL.into_iter().find(|s| s.name() == n)
    }

    /// The file an interactive shell of this kind reads last at startup.
    pub fn rc(self, env: &Env) -> PathBuf {
        match self {
            Shell::Bash => env.home.join(".bashrc"),
            Shell::Zsh => env.zdotdir.as_ref().unwrap_or(&env.home).join(".zshrc"),
            Shell::Fish => env
                .config
                .clone()
                .unwrap_or_else(|| env.home.join(".config"))
                .join("fish/config.fish"),
        }
    }

    /// The block that puts `dir` first on PATH.
    pub fn block(self, dir: &Path, home: &Path) -> String {
        let d = spell(dir, home);
        let line = match self {
            // --move puts it first even when PATH already has it.
            Shell::Fish => format!("fish_add_path --global --path --move \"{d}\""),
            Shell::Bash | Shell::Zsh => format!("export PATH=\"{d}:$PATH\""),
        };
        format!(
            "{BEGIN}\n# isbx's shims first on PATH, so they win over host programs and version\n# managers (`isb x update-shell --remove` takes this out).\n{line}\n{END}\n"
        )
    }
}

impl std::str::FromStr for Shell {
    type Err = Error;
    fn from_str(s: &str) -> Result<Shell> {
        Shell::ALL
            .into_iter()
            .find(|sh| sh.name() == s)
            .ok_or_else(|| Error::invalid(format!("{s}: not a shell isbx knows (bash, zsh, fish)")))
    }
}

/// What locates the startup files.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    /// `$ZDOTDIR`.
    pub zdotdir: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub config: Option<PathBuf>,
    /// `$SHELL`.
    pub login: Option<Shell>,
}

impl Env {
    pub fn from_process(home: PathBuf) -> Env {
        let var = |k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Env {
            home,
            zdotdir: var("ZDOTDIR"),
            config: var("XDG_CONFIG_HOME"),
            login: std::env::var("SHELL").ok().as_deref().and_then(Shell::of),
        }
    }

    /// The shells to edit: those asked for, else the login shell and every
    /// other one whose startup file exists.
    pub fn targets(&self, asked: &[Shell]) -> Result<Vec<Shell>> {
        let mut out: Vec<Shell> = if asked.is_empty() {
            Shell::ALL
                .into_iter()
                .filter(|s| Some(*s) == self.login || s.rc(self).exists())
                .collect()
        } else {
            asked.to_vec()
        };
        out.sort();
        out.dedup();
        if out.is_empty() {
            return Err(Error::invalid(
                "no bash, zsh or fish startup file found and $SHELL is none of them: name one with --shell",
            ));
        }
        Ok(out)
    }
}

/// `dir` as a startup file writes it: `$HOME/…` under the home directory,
/// with the characters double quotes do not protect escaped.
fn spell(dir: &Path, home: &Path) -> String {
    let esc = |s: &str| {
        s.chars().fold(String::new(), |mut o, c| {
            if matches!(c, '\\' | '"' | '$' | '`') {
                o.push('\\');
            }
            o.push(c);
            o
        })
    };
    match dir.strip_prefix(home) {
        Ok(rel) if !home.as_os_str().is_empty() => {
            format!("$HOME/{}", esc(&rel.to_string_lossy()))
        }
        _ => esc(&dir.to_string_lossy()),
    }
}

/// How a startup file stands.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// isbx's block is there.
    Block,
    /// A line of the user's own (1-based) already puts the directory on PATH.
    Line(usize),
    Absent,
}

/// Whether `text` puts `dir` on PATH.
pub fn state(text: &str, dir: &Path, home: &Path) -> State {
    if text.lines().any(|l| l.trim_end() == BEGIN) {
        return State::Block;
    }
    let abs = dir.to_string_lossy().into_owned();
    let mut spellings = vec![abs.clone()];
    if let Ok(rel) = dir.strip_prefix(home) {
        let rel = rel.to_string_lossy();
        spellings.extend([
            format!("$HOME/{rel}"),
            format!("${{HOME}}/{rel}"),
            format!("~/{rel}"),
        ]);
    }
    // The directory followed by the end of the name: not a longer path
    // that starts with it.
    let names = |l: &str| {
        spellings.iter().any(|s| {
            l.match_indices(s.as_str()).any(|(i, _)| {
                l[i + s.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| matches!(c, ':' | '"' | '\'' | ' ' | '/' | ';' | '\t'))
            })
        })
    };
    text.lines()
        .position(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && (t.contains("PATH") || t.contains("fish_add_path")) && names(t)
        })
        .map_or(State::Absent, |i| State::Line(i + 1))
}

/// `text` with the block for `shell` appended, or None when it already puts
/// `dir` on PATH.
pub fn add(text: &str, shell: Shell, dir: &Path, home: &Path) -> Option<String> {
    if state(text, dir, home) != State::Absent {
        return None;
    }
    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&shell.block(dir, home));
    Some(out)
}

/// `text` without isbx's block (and the blank line `add` put before it),
/// or None when there is none. A block without its end line is refused:
/// what follows it is the user's.
pub fn remove(text: &str) -> Result<Option<String>> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is = |l: &str, m: &str| l.trim_end() == m;
    let Some(begin) = lines.iter().position(|l| is(l, BEGIN)) else {
        return Ok(None);
    };
    let Some(end) = lines[begin..].iter().position(|l| is(l, END)) else {
        return Err(Error::invalid(format!(
            "`{BEGIN}` has no `{END}` after it; remove the block by hand"
        )));
    };
    let from = if begin > 0 && lines[begin - 1].trim().is_empty() {
        begin - 1
    } else {
        begin
    };
    let mut out: String = lines[..from].concat();
    out.push_str(&lines[begin + end + 1..].concat());
    Ok(Some(out))
}

/// What [`apply`] did to a file.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Added,
    Removed,
    /// Already as asked: the block is there (adding) or absent (removing).
    Unchanged,
    /// A line of the user's own puts the directory on PATH; isbx leaves it.
    UserLine(usize),
}

/// Add (or with `remove_it`, remove) the block in `path`, writing through a
/// symlink (dotfiles kept in a repository stay linked) and replacing the
/// file only once the new one is complete. A missing file is created when
/// adding.
pub fn apply(
    path: &Path,
    shell: Shell,
    dir: &Path,
    home: &Path,
    remove_it: bool,
) -> Result<Outcome> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::invalid(format!("{}: {e}", path.display()))),
    };
    let (new, done) = if remove_it {
        match remove(&text)? {
            Some(t) => (t, Outcome::Removed),
            None => {
                return Ok(match state(&text, dir, home) {
                    State::Line(n) => Outcome::UserLine(n),
                    _ => Outcome::Unchanged,
                });
            }
        }
    } else {
        match state(&text, dir, home) {
            State::Block => return Ok(Outcome::Unchanged),
            State::Line(n) => return Ok(Outcome::UserLine(n)),
            State::Absent => (
                add(&text, shell, dir, home).unwrap_or_default(),
                Outcome::Added,
            ),
        }
    };
    write(path, &new)?;
    Ok(done)
}

fn write(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let target = if path.is_symlink() {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mode = std::fs::metadata(&target).map_or(0o644, |m| m.permissions().mode());
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{name}.isbx-tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";
    const DIR: &str = "/home/u/.local/share/isb/x/bin";

    fn st(text: &str) -> State {
        state(text, Path::new(DIR), Path::new(HOME))
    }

    #[test]
    fn the_block_spells_the_directory_under_home() {
        let b = Shell::Bash.block(Path::new(DIR), Path::new(HOME));
        assert!(b.starts_with(BEGIN) && b.ends_with(&format!("{END}\n")));
        assert!(b.contains("export PATH=\"$HOME/.local/share/isb/x/bin:$PATH\"\n"));
        let f = Shell::Fish.block(Path::new(DIR), Path::new(HOME));
        assert!(
            f.contains("fish_add_path --global --path --move \"$HOME/.local/share/isb/x/bin\"\n")
        );
        let elsewhere = Shell::Zsh.block(Path::new("/opt/a$b/bin"), Path::new(HOME));
        assert!(elsewhere.contains("export PATH=\"/opt/a\\$b/bin:$PATH\""));
    }

    #[test]
    fn a_users_own_line_counts_in_every_spelling() {
        // titan's ~/.bashrc, written by hand before isbx could do it.
        let titan = "# isbx tool shims (isb x install); first on PATH so shims win over host programs\nexport PATH=\"$HOME/.local/share/isb/x/bin:$PATH\"\n";
        assert_eq!(st(titan), State::Line(2));
        for l in [
            "export PATH=/home/u/.local/share/isb/x/bin:$PATH",
            "PATH=\"${HOME}/.local/share/isb/x/bin:${PATH}\"",
            "  export PATH=~/.local/share/isb/x/bin:$PATH",
            "fish_add_path $HOME/.local/share/isb/x/bin",
            "set -gx PATH /home/u/.local/share/isb/x/bin $PATH",
        ] {
            assert_eq!(st(l), State::Line(1), "{l}");
        }
        for l in [
            "# export PATH=\"$HOME/.local/share/isb/x/bin:$PATH\"",
            "ls ~/.local/share/isb/x/bin",
            "export PATH=\"$HOME/.local/share/isb/x/binaries:$PATH\"",
            "export PATH=\"$HOME/.local/bin:$PATH\"",
        ] {
            assert_eq!(st(l), State::Absent, "{l}");
        }
    }

    #[test]
    fn adding_is_idempotent_and_removing_restores_the_file() {
        let (dir, home) = (Path::new(DIR), Path::new(HOME));
        for before in ["", "alias ll='ls -l'\n", "no newline at the end"] {
            let after = add(before, Shell::Bash, dir, home).unwrap();
            assert!(after.starts_with(before));
            assert_eq!(st(&after), State::Block);
            assert_eq!(add(&after, Shell::Bash, dir, home), None);
            let back = remove(&after).unwrap().unwrap();
            let expect = if before.is_empty() || before.ends_with('\n') {
                before.to_string()
            } else {
                format!("{before}\n")
            };
            assert_eq!(back, expect);
            assert_eq!(remove(&back).unwrap(), None);
        }
        // Lines after the block stay.
        let mid = format!("a\n{}b\n", Shell::Zsh.block(dir, home));
        assert_eq!(remove(&mid).unwrap().unwrap(), "a\nb\n");
        assert!(remove(&format!("{BEGIN}\nexport PATH=x\n")).is_err());
    }

    #[test]
    fn apply_creates_finds_and_leaves_user_lines() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let dir = home.join(".local/share/isb/x/bin");
        let fish = home.join(".config/fish/config.fish");
        // Missing file and parent directory: created.
        assert_eq!(
            apply(&fish, Shell::Fish, &dir, home, false).unwrap(),
            Outcome::Added
        );
        assert_eq!(
            apply(&fish, Shell::Fish, &dir, home, false).unwrap(),
            Outcome::Unchanged
        );
        assert_eq!(
            apply(&fish, Shell::Fish, &dir, home, true).unwrap(),
            Outcome::Removed
        );
        assert_eq!(std::fs::read_to_string(&fish).unwrap(), "");
        assert_eq!(
            apply(&fish, Shell::Fish, &dir, home, true).unwrap(),
            Outcome::Unchanged
        );

        let rc = home.join(".bashrc");
        std::fs::write(
            &rc,
            "x\nexport PATH=\"$HOME/.local/share/isb/x/bin:$PATH\"\n",
        )
        .unwrap();
        assert_eq!(
            apply(&rc, Shell::Bash, &dir, home, false).unwrap(),
            Outcome::UserLine(2)
        );
        assert_eq!(
            apply(&rc, Shell::Bash, &dir, home, true).unwrap(),
            Outcome::UserLine(2)
        );

        // A symlinked rc stays a symlink; its target gets the block.
        let real = home.join("dotfiles-zshrc");
        std::fs::write(&real, "setopt x\n").unwrap();
        let link = home.join(".zshrc");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            apply(&link, Shell::Zsh, &dir, home, false).unwrap(),
            Outcome::Added
        );
        assert!(link.is_symlink());
        assert_eq!(
            state(&std::fs::read_to_string(&real).unwrap(), &dir, home),
            State::Block
        );
    }

    #[test]
    fn targets_are_the_login_shell_and_existing_files() {
        let t = tempfile::tempdir().unwrap();
        let mut env = Env {
            home: t.path().to_path_buf(),
            zdotdir: None,
            config: None,
            login: Shell::of("/usr/bin/zsh"),
        };
        assert_eq!(env.targets(&[]).unwrap(), vec![Shell::Zsh]);
        std::fs::write(t.path().join(".bashrc"), "").unwrap();
        assert_eq!(env.targets(&[]).unwrap(), vec![Shell::Bash, Shell::Zsh]);
        assert_eq!(env.targets(&[Shell::Fish]).unwrap(), vec![Shell::Fish]);
        env.login = None;
        std::fs::remove_file(t.path().join(".bashrc")).unwrap();
        assert!(env.targets(&[]).is_err());
        env.zdotdir = Some(t.path().join("z"));
        assert_eq!(Shell::Zsh.rc(&env), t.path().join("z/.zshrc"));
    }
}
