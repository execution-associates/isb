# Developing isb

Build, test and run isb inside a sandbox, not on a host that holds
credentials: a build script or a dependency runs with whatever the shell it
starts in can reach. The incus socket is root-equivalent, so the integration
tests are compiled in the sandbox (`cargo test --no-run`) and run on the host
(see the README).

In a sandbox with the worktree bind-mounted, point `CARGO_TARGET_DIR` at the
sandbox's own filesystem (for example `/home/dev/.cache/isb-target`), not at
the worktree: a debug build of the workspace is tens of gigabytes, and there
it goes away with the sandbox instead of piling up on the host. Copy out
only what the host needs, such as an integration test binary.

## Layout

isb is a cargo workspace. The `isb` package at the root is the CLI
(`src/bin/isb.rs` and `src/bin/isb/`) and the library everyone depends on,
which re-exports the internal crates' modules under their original paths
(`isb::spec`, `isb::daemon`, ...). The internal crates, in build order:

| crate | modules |
|---|---|
| `crates/isb-core` | the incus client, spec, plan, sandbox, compose, stack, org, registry, ingress, secrets, rpc, machine, metrics, net (outbound connections under the SSRF policy), `serve_client` (the CLI's client for `isb serve`) |
| `crates/isb-server` | auth, audit, history, server, servers, web (its `build.rs` embeds `web/dist`) |
| `crates/isb-apps` | app, build, jobs, backup, s3, template, notify, volume_backup |
| `crates/isb-tui` | tui |
| `crates/isb-daemon` | daemon, workspace |

`isb-server`, `isb-apps` and `isb-tui` depend only on `isb-core`, so they
compile in parallel; `isb-daemon` needs all but the TUI. An edit recompiles
its own crate and the crates above it, so a change to the TUI or the CLI
never re-checks the other 90k lines. Each internal crate's root imports the
modules of the crates below it (`use isb_core::*;`), so `crate::org::OrgId`
means the same in every crate. An item one crate needs from a lower one is
`#[doc(hidden)] pub`, which keeps it out of `isb`'s documented API.

The internal crates are published with `isb` and share its version
(`[workspace.package]`, and the `=` versions in `[workspace.dependencies]`).
`cargo package` and `cargo publish --workspace` handle them in dependency
order.

## The checks

```sh
scripts/check.sh           # ratchet, fmt, clippy -D warnings, unit tests
scripts/check.sh --quick   # the same without the tests
```

Plain `cargo test`, `cargo clippy`, `cargo doc` and `cargo build` cover the
whole workspace (`default-members`). CI runs the same steps
(`.github/workflows/ci.yml` and `build-health.yml`).

## The legibility ratchet

New code may not be harder to read than the rule, and old code may get
better but never worse.

- **Clippy thresholds** (`clippy.toml`, lints enabled for every crate in the
  root `Cargo.toml`'s `[workspace.lints.clippy]`, denied in CI):

  | lint | limit |
  |---|---|
  | `too_many_lines` | 100 lines per function (comments and blanks excluded) |
  | `cognitive_complexity` | 25 |
  | `excessive_nesting` | 6 nested blocks |
  | `too_many_arguments` | 7 |
  | `type_complexity` | 250 |

  The thresholds are the targets, not today's worst case: a threshold set
  just above the worst function would let new code grow to that size. Each
  function that already exceeded one carries
  `#[expect(clippy::<lint>, reason = "predates the lint ratchet; ...")]`.
  An `expect` that no longer fires is itself a warning
  (`unfulfilled_lint_expectations`), so once such a function is split under
  the limit, clippy insists the exemption goes too.
- **Exemption budget**: `scripts/ratchet.sh` counts how often each of those
  lints is named in the source and fails when a count exceeds its budget in
  `scripts/ratchet.txt`, so a new exemption cannot slip in unnoticed.
- **File size**: the same script fails any `.rs` file over 1000 lines, except
  the files listed in `scripts/ratchet.txt`, each capped at its listed size.

Both budgets go down only. After splitting a long function or file, run
`scripts/ratchet.sh --update` to lower them (it never raises one) and commit
the new `scripts/ratchet.txt`. Raising a budget is a hand edit a reviewer
sees; splitting is almost always the better fix. When you change a function
that carries an exemption, split it and drop the `#[expect]`.

## Compile time

`scripts/build-times.sh` measures the edit-compile loop: each of `cargo
check`, a debug build, `cargo test --no-run`, `cargo clippy` and a release
musl build, after a one-line edit in a leaf module and in a widely used one.

```sh
scripts/build-times.sh                 # check build test clippy
scripts/build-times.sh release         # minutes per cell
RUNS=1 scripts/build-times.sh check    # one run per cell (default: best of 3)
```

Debug builds keep line tables only (backtraces have file:line; set `debug =
true` in `[profile.dev]` locally to step through code) and dependencies carry
no debuginfo. The linker is the toolchain's default: on x86_64 Linux that is
rust-lld since Rust 1.90. Linking is about 0.7 s of a 5 s edit-and-build, and
mold saved 0.3 s of it, too little to make every contributor and CI job
install it; to use it anyway, set it in your own `~/.cargo/config.toml`
(`[target.x86_64-unknown-linux-gnu]` with `linker = "clang"` and
`rustflags = ["-C", "link-arg=-fuse-ld=mold"]`).

To see where a full build spends its time, `cargo build --timings` writes
`cargo-timings/cargo-timing.html` under the target directory (CI's `build timings` job uploads
one per run). To find monomorphization bloat, `cargo llvm-lines` lists the
functions that generate the most LLVM IR, generic instantiations summed:

```sh
mise use -g cargo:cargo-llvm-lines     # inside the sandbox
cargo llvm-lines --lib | head -40
```

Serde derives dominate it: every `Deserialize` type is instantiated once per
deserializer it meets (`serde_json::from_slice` and `from_str` are two,
`from_value` a third, YAML two more), so parse a type through one entry
point where you can.
