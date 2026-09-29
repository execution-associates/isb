//! Foreground `isb up`: run each sandbox's `command`, stream its output, and
//! stop the sandboxes when it is over.
//!
//! "Over" is any of: every command has exited, SIGINT/SIGTERM/SIGHUP, stdout
//! went away (the reader of a pipe exited), or a process that started isb
//! exited. The last one is what signals cannot give: an agent's background task
//! or a closed terminal can end without ever signalling its descendants, which
//! otherwise leaves the sandbox (and its published ports) running with nobody
//! attached. isb records its ancestors at startup and polls them.

use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;

use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

use crate::error::Result;
use crate::exec::{ExecEvent, ExecOptions};
use crate::sandbox::Sandbox;

/// One sandbox to hold in the foreground.
pub struct Service {
    /// Service name, used as the log prefix.
    pub name: String,
    pub sandbox: Sandbox,
    /// Its `command`; `None` holds the sandbox until something else ends the run.
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Prefix each output line with `<service> | `.
    pub log_prefix: bool,
    /// How long a clean shutdown may take before the sandbox is killed.
    pub stop_timeout: Duration,
    /// How often to check that the processes that started isb are still alive.
    pub poll: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            log_prefix: true,
            stop_timeout: Duration::from_secs(10),
            poll: Duration::from_secs(1),
        }
    }
}

enum Msg {
    Signal(i32),
    Orphaned(u32),
    OutputClosed,
    Exited(usize, Result<i32>),
    Stopped,
}

/// Run the commands and hold the sandboxes until the run is over, then stop
/// them. Returns the exit code: the first failing command's status (0 if all
/// succeeded), 128+N for signal N, 129 when a parent process went away, and
/// 141 when stdout closed.
pub fn run(services: &[Service], opts: Options, report: &mut dyn FnMut(&str)) -> Result<u8> {
    let (tx, rx) = mpsc::channel::<Msg>();

    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let sig_handle = signals.handle();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for sig in signals.forever() {
                if tx.send(Msg::Signal(sig)).is_err() {
                    break;
                }
            }
        });
    }

    let ancestors = ancestors();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(opts.poll);
                if let Some(&(pid, _)) = ancestors.iter().find(|&&(p, t)| !alive(p, t)) {
                    let _ = tx.send(Msg::Orphaned(pid));
                    break;
                }
            }
        });
    }

    let mut running = 0usize;
    for (i, svc) in services.iter().enumerate() {
        let Some(argv) = &svc.command else { continue };
        let stream = match svc
            .sandbox
            .exec_stream(argv.clone(), ExecOptions::default())
        {
            Ok(s) => s,
            Err(e) => {
                report(&format!("{}: command failed to start: {e}", svc.name));
                let _ = tx.send(Msg::Exited(i, Err(e)));
                running += 1;
                continue;
            }
        };
        running += 1;
        let prefix = opts.log_prefix.then(|| format!("{} | ", svc.name));
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut out = LineWriter::new(prefix.clone());
            let mut err = LineWriter::new(prefix);
            let mut stream = stream;
            let mut closed = false;
            while let Some(ev) = stream.next_event() {
                let r = match ev {
                    ExecEvent::Stdout(b) => out.write(&mut io::stdout().lock(), &b),
                    ExecEvent::Stderr(b) => err.write(&mut io::stderr().lock(), &b),
                };
                if r.is_err() && !closed {
                    closed = true;
                    let _ = tx.send(Msg::OutputClosed);
                }
            }
            let _ = out.finish(&mut io::stdout().lock());
            let _ = err.finish(&mut io::stderr().lock());
            let _ = tx.send(Msg::Exited(i, stream.wait()));
        });
    }

    if running == 0 {
        let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
        report(&format!(
            "{}: up; Ctrl-C stops (no command, so nothing else will)",
            names.join(", ")
        ));
    }

    let mut first_failure: Option<i32> = None;
    let code: u8 = loop {
        match rx.recv() {
            Ok(Msg::Exited(i, r)) => {
                let name = &services[i].name;
                match r {
                    Ok(c) => {
                        report(&format!("{name}: command exited with code {c}"));
                        if c != 0 && first_failure.is_none() {
                            first_failure = Some(c);
                        }
                    }
                    Err(e) => {
                        report(&format!("{name}: command failed: {e}"));
                        first_failure.get_or_insert(1);
                    }
                }
                running -= 1;
                if running == 0 {
                    break first_failure.map_or(0, |c| c.clamp(1, 255) as u8);
                }
            }
            Ok(Msg::Signal(s)) => break 128 + s as u8,
            Ok(Msg::Orphaned(pid)) => {
                report(&format!("process {pid}, which started isb, has exited"));
                break 129;
            }
            Ok(Msg::OutputClosed) => break 141,
            Ok(Msg::Stopped) => unreachable!("stop has not started"),
            Err(_) => break 1,
        }
    };

    // A clean stop, in the background, so a second Ctrl-C can force it.
    let handles: Vec<Sandbox> = services.iter().map(|s| s.sandbox.clone()).collect();
    let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
    for n in &names {
        report(&format!("{n}: stopping"));
    }
    {
        let tx = tx.clone();
        let handles = handles.clone();
        std::thread::spawn(move || {
            for sb in &handles {
                if sb.stop(false, opts.stop_timeout).is_err() {
                    let _ = sb.stop(true, opts.stop_timeout);
                }
            }
            let _ = tx.send(Msg::Stopped);
        });
    }
    loop {
        match rx.recv() {
            Ok(Msg::Stopped) | Err(_) => break,
            Ok(Msg::Signal(_)) => {
                report("forcing stop");
                for sb in &handles {
                    let _ = sb.stop(true, opts.stop_timeout);
                }
                break;
            }
            Ok(_) => {}
        }
    }
    for n in &names {
        report(&format!("{n}: stopped"));
    }
    sig_handle.close();
    Ok(code)
}

/// Writes output with a prefix at the start of every line.
struct LineWriter {
    prefix: Option<String>,
    at_line_start: bool,
}

impl LineWriter {
    fn new(prefix: Option<String>) -> Self {
        LineWriter {
            prefix,
            at_line_start: true,
        }
    }

    fn write(&mut self, w: &mut impl Write, buf: &[u8]) -> io::Result<()> {
        let Some(prefix) = &self.prefix else {
            w.write_all(buf)?;
            return w.flush();
        };
        for line in buf.split_inclusive(|&b| b == b'\n') {
            if self.at_line_start {
                w.write_all(prefix.as_bytes())?;
            }
            w.write_all(line)?;
            self.at_line_start = line.ends_with(b"\n");
        }
        w.flush()
    }

    /// End a trailing partial line.
    fn finish(&mut self, w: &mut impl Write) -> io::Result<()> {
        if self.prefix.is_some() && !self.at_line_start {
            self.at_line_start = true;
            w.write_all(b"\n")?;
            return w.flush();
        }
        Ok(())
    }
}

/// This process's ancestors below pid 1, as (pid, start time) so a reused pid
/// is not mistaken for the original.
fn ancestors() -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    let Some((mut ppid, _)) = stat("self") else {
        return out;
    };
    while ppid > 1 && out.len() < 64 {
        let Some((next, start)) = stat(&ppid.to_string()) else {
            break;
        };
        out.push((ppid, start));
        ppid = next;
    }
    out
}

fn alive(pid: u32, start: u64) -> bool {
    matches!(stat(&pid.to_string()), Some((_, s)) if s == start)
}

/// (ppid, start time) from `/proc/<pid>/stat`; `None` for a missing or
/// zombie process.
fn stat(pid: &str) -> Option<(u32, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&s)
}

fn parse_stat(s: &str) -> Option<(u32, u64)> {
    // The command name can contain spaces and parentheses: fields resume after
    // the LAST ')'. Then: state ppid ... with starttime the 20th after it.
    let rest = &s[s.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    if matches!(f.first(), Some(&"Z") | Some(&"X")) {
        return None;
    }
    Some((f.get(1)?.parse().ok()?, f.get(19)?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat() {
        let line =
            "1234 (my (odd) cmd) S 99 1234 1234 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 5555 0 0";
        assert_eq!(parse_stat(line), Some((99, 5555)));
        let zombie = "1234 (x) Z 99 1234 1234 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 5555 0 0";
        assert_eq!(parse_stat(zombie), None);
    }

    #[test]
    fn own_ancestors_are_alive() {
        let a = ancestors();
        assert!(!a.is_empty());
        assert!(a.iter().all(|&(p, t)| alive(p, t)));
        assert!(!alive(a[0].0, a[0].1 + 1));
    }

    #[test]
    fn prefixes_every_line() {
        let mut w = LineWriter::new(Some("web | ".into()));
        let mut out = Vec::new();
        w.write(&mut out, b"one\ntw").unwrap();
        w.write(&mut out, b"o\nthree").unwrap();
        w.finish(&mut out).unwrap();
        assert_eq!(out, b"web | one\nweb | two\nweb | three\n");
    }
}
