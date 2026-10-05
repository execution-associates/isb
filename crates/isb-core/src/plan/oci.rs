//! What an OCI instance needs beyond config: its command line, and work
//! done on it between creation and first start.
//!
//! The command line: incus writes `oci.entrypoint` into the line-based LXC
//! config and splits it on whitespace with quotes grouping. There is no
//! escape character, so a line break cannot be carried at all, and an
//! argument may hold only one kind of quote. The common case that needs
//! more, `sh -c SCRIPT`, is rewritten so the
//! shell decodes the script itself: `eval "$(printf %b "...")"`, with every
//! line break, quote, `$`, backtick and backslash written as a `printf %b`
//! escape. `$0` and the positional arguments are the shell's, as with `-c`.
//! Anything else that cannot be carried is refused.

use crate::error::Result;

/// See [`Desired::before_start`]: called with the instance's name.
#[derive(Clone)]
pub struct BeforeStart(pub std::sync::Arc<BeforeStartFn>);

/// The work: given a client on the instance's project, and its name.
pub type BeforeStartFn = dyn Fn(&crate::client::Client, &str) -> Result<()> + Send + Sync;

impl std::fmt::Debug for BeforeStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BeforeStart")
    }
}

/// Shells whose `-c` script may be rewritten (by basename).
const SHELLS: &[&str] = &["sh", "bash", "dash", "ash", "zsh", "ksh", "mksh"];

/// Quote argv for `oci.entrypoint`. An argument that fits on the line is
/// written as before, so the result for one is byte-identical.
pub fn oci_command_line(argv: &[String]) -> std::result::Result<String, String> {
    let script = shell_script_index(argv);
    argv.iter()
        .enumerate()
        .map(|(i, a)| {
            if fits(a) {
                return Ok(quote(a));
            }
            if Some(i) == script {
                return Ok(quote(&shell_eval(a)));
            }
            if a.contains(['\n', '\r']) {
                Err(format!(
                    "argument {a:?} has a line break, which an OCI command line cannot carry. \
                     Only the script of `sh -c SCRIPT` (or bash, ash, dash, zsh, ksh) may span \
                     lines; put the text in a file or pass it through such a shell"
                ))
            } else {
                Err(format!(
                    "argument {a:?} has both ' and \" in it, which an OCI command line cannot \
                     carry; use `sh -c SCRIPT` (isb rewrites a shell script) or a file"
                ))
            }
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(|v| v.join(" "))
}

/// Refuse, when a file is loaded, an OCI command line that cannot be written.
pub fn check_oci_command(
    service: &str,
    spec: &crate::spec::SandboxSpec,
) -> std::result::Result<(), String> {
    if !super::ImageSource::parse(&spec.image).is_ok_and(|i| i.is_oci()) {
        return Ok(());
    }
    let mut line = spec.entrypoint.clone().unwrap_or_default();
    line.extend(spec.command.clone().unwrap_or_default());
    oci_command_line(&line)
        .map(|_| ())
        .map_err(|e| format!("service {service:?}: {e}"))
}

/// Whether `oci.entrypoint` can hold `a` as it is.
fn fits(a: &str) -> bool {
    !a.contains(['\n', '\r']) && !(a.contains('"') && a.contains('\''))
}

fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        a.to_string()
    } else if !a.contains('"') {
        format!("\"{a}\"")
    } else {
        format!("'{a}'")
    }
}

/// The index of the script in `<shell> [OPTIONS] -c SCRIPT ...`, if argv has
/// that shape (`-ec`, `-lc` and `-o OPT` included).
fn shell_script_index(argv: &[String]) -> Option<usize> {
    let sh = argv.iter().position(|a| {
        let base = a.rsplit('/').next().unwrap_or(a);
        SHELLS.contains(&base)
    })?;
    let mut i = sh + 1;
    while let Some(a) = argv.get(i) {
        let flags = a.strip_prefix('-').or_else(|| a.strip_prefix('+'))?;
        if flags.is_empty() || flags.starts_with('-') {
            return None;
        }
        if flags == "o" || flags == "O" {
            i += 2;
            continue;
        }
        if a.starts_with('-') && flags.contains('c') {
            return (i + 1 < argv.len()).then_some(i + 1);
        }
        i += 1;
    }
    None
}

/// `script` as a one-line `-c` argument that only holds `"`: the shell's own
/// `printf %b` turns the escapes back into the script, which `eval` runs.
fn shell_eval(script: &str) -> String {
    let mut esc = String::with_capacity(script.len() + 16);
    for c in script.chars() {
        // Inside "...", `\\` is one backslash, so `\\n` reaches printf as `\n`.
        match c {
            '\n' => esc.push_str("\\\\n"),
            '\r' => esc.push_str("\\\\r"),
            '"' => esc.push_str("\\\\0042"),
            '$' => esc.push_str("\\\\0044"),
            '\'' => esc.push_str("\\\\0047"),
            '\\' => esc.push_str("\\\\0134"),
            '`' => esc.push_str("\\\\0140"),
            c => esc.push(c),
        }
    }
    format!("eval \"$(printf %b \"{esc}\")\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_line_arguments_are_written_as_before() {
        let l = oci_command_line(&argv(&["sh", "-c", "echo $HOME; exec app", "", "a'b"])).unwrap();
        assert_eq!(l, r#"sh -c "echo $HOME; exec app" "" "a'b""#);
        let l = oci_command_line(&argv(&["app", "--x=\"y z\""])).unwrap();
        assert_eq!(l, r#"app '--x="y z"'"#);
    }

    #[test]
    fn a_multi_line_shell_script_becomes_one_line() {
        let script = "set -e\necho \"$1\" 'x' `id` \\\nexec app";
        let l = oci_command_line(&argv(&["/bin/sh", "-ec", script, "name", "arg"])).unwrap();
        assert!(!l.contains('\n'), "{l}");
        assert_eq!(
            l,
            "/bin/sh -ec 'eval \"$(printf %b \"set -e\\\\necho \\\\0042\\\\00441\\\\0042 \
             \\\\0047x\\\\0047 \\\\0140id\\\\0140 \\\\0134\\\\nexec app\")\"' name arg"
        );
    }

    #[test]
    fn shell_options_before_c_are_skipped() {
        let a = argv(&["tini", "--", "bash", "-o", "pipefail", "-lc", "a\nb"]);
        assert_eq!(shell_script_index(&a), Some(6));
        assert_eq!(
            shell_script_index(&argv(&["bash", "--login", "-c", "x"])),
            None
        );
        assert_eq!(shell_script_index(&argv(&["sh", "-c"])), None);
        assert_eq!(
            shell_script_index(&argv(&["sh", "script.sh", "-c", "x"])),
            None
        );
        assert_eq!(
            shell_script_index(&argv(&["busybox", "sh", "-c", "x"])),
            Some(3)
        );
    }

    #[test]
    fn a_line_break_elsewhere_is_refused() {
        let e = oci_command_line(&argv(&["app", "--config", "a\nb"])).unwrap_err();
        assert!(e.contains("line break"), "{e}");
        let e = oci_command_line(&argv(&["sh", "-c", "echo", "a\nb"])).unwrap_err();
        assert!(e.contains("line break"), "{e}");
        let e = oci_command_line(&argv(&["python", "-c", "a\nb"])).unwrap_err();
        assert!(e.contains("line break"), "{e}");
    }

    #[test]
    fn both_quotes_in_a_shell_script_are_carried() {
        let l = oci_command_line(&argv(&["sh", "-c", r#"echo "it's""#])).unwrap();
        assert!(l.starts_with("sh -c 'eval "), "{l}");
        let e = oci_command_line(&argv(&["app", r#""it's""#])).unwrap_err();
        assert!(e.contains("both ' and \""), "{e}");
    }

    #[test]
    fn loading_a_file_refuses_what_cannot_be_written() {
        let load = |doc: &str| {
            let docs = [(std::path::PathBuf::from("f.yaml"), doc.to_string())];
            crate::compose::load_docs(&docs, std::path::Path::new("/tmp/p"), None, &|_| None)
        };
        let e = load("services:\n  web: {image: docker:busybox, command: [app, \"a\\nb\"]}\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("\"web\"") && e.contains("line break"), "{e}");
        load("services:\n  web: {image: docker:busybox, command: [sh, -c, \"a\\nb\"]}\n").unwrap();
        // A system image's command is a unit's ExecStart, not this line.
        load("services:\n  web: {image: dev-base, command: [app, \"a\\nb\"]}\n").unwrap();
    }

    /// The rewritten script means the same to real shells (when present).
    #[test]
    fn shells_decode_the_rewritten_script() {
        let script = "set -e\nprintf '%s|' \"$0\" \"$@\" 'a\"b' \"c'd\" `echo e` \\\n  f\ncat <<EOF\nx $1 \\$y\nEOF";
        for sh in ["sh", "dash", "bash", "busybox"] {
            let run = |s: &str| {
                let mut cmd = std::process::Command::new(sh);
                if sh == "busybox" {
                    cmd.arg("sh");
                }
                cmd.args(["-c", s, "zero", "one", "t w o"]).output()
            };
            let (Ok(out), Ok(direct)) = (run(&shell_eval(script)), run(script)) else {
                continue;
            };
            assert!(
                out.status.success(),
                "{sh}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "zero|one|t w o|a\"b|c'd|e|f|x one $y\n",
                "{sh}"
            );
            assert_eq!(out.stdout, direct.stdout, "{sh}");
        }
    }
}
