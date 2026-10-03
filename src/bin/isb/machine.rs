//! `isb machine ...`: the Lima VM that runs incus on a Mac.

use super::*;

#[derive(Subcommand)]
pub(crate) enum MachineCmd {
    /// Create and start the machine: Ubuntu 24.04 with incus, $HOME shared at
    /// the same path, its sockets forwarded under ~/.isb/machine/NAME, and
    /// isb serve running inside.
    Init {
        /// Machine name (also the Lima instance's).
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(long, default_value = "4")]
        cpus: u32,
        #[arg(long, default_value = "4GiB")]
        memory: String,
        /// The VM disk's maximum size (it grows as used).
        #[arg(long, default_value = "10GiB")]
        disk: String,
        /// A Linux (musl) isb binary for the guest, instead of downloading
        /// this version's release.
        #[arg(long)]
        isb_binary: Option<PathBuf>,
        /// Deadline for the first boot (image download, incus install).
        #[arg(long, value_parser = dur, default_value = "20m")]
        timeout: Duration,
    },
    /// Start a stopped machine and wait until incus and isb serve answer.
    Start {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// Stop the machine (sandboxes and stacks in it stop too).
    Stop {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// Delete the machine, everything in it, and a LaunchAgent that starts it.
    Rm {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
    },
    /// The machine's state, resources and sockets.
    Status {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// A shell in the machine, or a command: `isb machine ssh [NAME] -- CMD...`.
    Ssh {
        #[arg(default_value = isb::machine::DEFAULT_NAME)]
        name: String,
        #[arg(last = true)]
        command: Vec<String>,
    },
}

pub(crate) fn machine(ctx: &Ctx, cmd: MachineCmd) -> Result<u8> {
    use isb::machine as m;
    let log = |s: &str| {
        if !ctx.global.quiet {
            eprintln!("isb machine: {s}");
        }
    };
    match cmd {
        MachineCmd::Init {
            name,
            cpus,
            memory,
            disk,
            isb_binary,
            timeout,
        } => {
            let st = m::init(
                &m::InitOptions {
                    name,
                    cpus,
                    memory,
                    disk,
                    isb_binary,
                    timeout,
                },
                &log,
            )?;
            print_machine(&st);
        }
        MachineCmd::Start { name } => {
            m::start(&name)?;
            log(&format!("{name} is running"));
        }
        MachineCmd::Stop { name } => m::stop(&name)?,
        MachineCmd::Rm { name } => {
            m::remove(&name)?;
            log(&format!("removed {name}"));
        }
        MachineCmd::Status { name, json } => {
            let st = m::status(&name)?;
            if json {
                print_json(&st);
            } else {
                print_machine(&st);
            }
        }
        MachineCmd::Ssh { name, command } => {
            use std::os::unix::process::CommandExt;
            let err = m::shell_command(&name, &command)?.exec();
            return Err(Error::Invalid(format!("limactl shell {name}: {err}")));
        }
    }
    Ok(0)
}

pub(crate) fn print_machine(st: &isb::machine::Status) {
    let gib = |b: Option<u64>| {
        b.map(|b| format!("{:.1}GiB", b as f64 / (1u64 << 30) as f64))
            .unwrap_or_else(|| "-".into())
    };
    let default = if st.default { " (default)" } else { "" };
    println!("machine    {}{default}", st.name);
    println!("state      {}", st.state);
    println!(
        "resources  {} cpus, {} memory, {} disk{}",
        st.cpus.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
        gib(st.memory_bytes),
        gib(st.disk_bytes),
        st.arch
            .as_deref()
            .map(|a| format!(", {a}"))
            .unwrap_or_default()
    );
    let incus = match (&st.incus_version, &st.incus_error) {
        (Some(v), _) => format!("incus {v}"),
        (None, Some(e)) => format!("not answering ({e})"),
        (None, None) => "not running".into(),
    };
    println!("incus      {} at {}", incus, st.incus_socket.display());
    println!(
        "isb serve  {} at {}, http://{}",
        if st.serve_ok { "ok" } else { "not answering" },
        st.serve_socket.display(),
        st.serve_listen
    );
    if !st.default {
        println!(
            "use it with: export INCUS_SOCKET={} ISB_SERVE_SOCKET={}",
            st.incus_socket.display(),
            st.serve_socket.display()
        );
    }
}
