---
title: Developing isb
description: How the repository is laid out, how to build and test isb safely in a sandbox, the checks CI runs, the legibility ratchet, and measuring compile time.
order: 6
nav_title: Contributing
---

isb is a Rust cargo workspace with a React web UI and two thin SDKs. This
section is for people changing isb itself; using it is covered everywhere
else. Developing the web UI has [its own page](web-ui.md).

## Build and test in a sandbox

Build, test and run isb inside a sandbox, not on a host that holds
credentials: a build script or a dependency runs with whatever the shell it
starts in can reach. isb is a good tool for that sandbox. Mount only the
checkout, never your home directory, and never the incus socket, which is
root-equivalent on the host.

In a sandbox with the worktree bind-mounted, point `CARGO_TARGET_DIR` at the
sandbox's own filesystem (for example `/home/dev/.cache/isb-target`), not at
the worktree: a debug build of the workspace is tens of gigabytes, and there
it goes away with the sandbox instead of piling up on the host. Copy out only
what the host needs, such as an integration test binary.

```sh
cargo test                          # unit tests; integration tests skip themselves
ISB_INTEGRATION=1 cargo test        # against a real incusd
```

The integration tests need a local image with a `dev` user at uid 1000 and
`python3` (`ISB_TEST_IMAGE`, default `dev-base`). Everything they create is
named `isb-test-*` and is removed afterwards, pass or fail. Because they need
the incus socket, build the test binaries in the sandbox without it
(`cargo test --no-run`) and run them on the host.

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

Beside the Rust code: `web/` is the web UI ([Developing the web UI](web-ui.md)),
`sdk/python` and `sdk/typescript` are the SDKs (both speak
[the rpc protocol](../reference/rpc.md)), `examples/` holds real compose
files, and `docs/` is this documentation. `docs/design/` holds proposals,
which describe plans rather than what isb does.

## The checks

```sh
scripts/check.sh           # ratchet, fmt, clippy -D warnings, docs links, unit tests
scripts/check.sh --quick   # the same without the tests
```

Plain `cargo test`, `cargo clippy`, `cargo doc` and `cargo build` cover the
whole workspace (`default-members`). CI runs the same steps
(`.github/workflows/ci.yml` and `build-health.yml`), plus `cargo doc` with
warnings denied, a `cargo package` dry run, a macOS build and test on arm64
and x86_64, the minimum supported Rust version (1.85), and the web UI's lint,
tests and build.

## The legibility ratchet

New code may not be harder to read than the rule, and old code may get better
but never worse.

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
  `#[expect(clippy::<lint>, reason = "predates the lint ratchet; ...")]`. An
  `expect` that no longer fires is itself a warning
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
rust-lld (Rust 1.90 and later). Linking is about 0.7 s of a 5 s edit-and-build, and mold saved 0.3 s
of it, too little to make every contributor and CI job install it; to use it
anyway, set it in your own `~/.cargo/config.toml`
(`[target.x86_64-unknown-linux-gnu]` with `linker = "clang"` and
`rustflags = ["-C", "link-arg=-fuse-ld=mold"]`).

To see where a full build spends its time, `cargo build --timings` writes
`cargo-timings/cargo-timing.html` under the target directory (CI's `build
timings` job uploads one per run). To find monomorphization bloat, `cargo
llvm-lines` lists the functions that generate the most LLVM IR, generic
instantiations summed:

```sh
mise use -g cargo:cargo-llvm-lines     # inside the sandbox
cargo llvm-lines --lib | head -40
```

Serde derives dominate it: every `Deserialize` type is instantiated once per
deserializer it meets (`serde_json::from_slice` and `from_str` are two,
`from_value` a third, YAML two more), so parse a type through one entry point
where you can.

## Documentation

Pages under `docs/` are GitHub-flavored Markdown with front matter (`title`,
`description`, optional `order` and `nav_title`); links between them are
relative `.md` paths. `scripts/check.sh` checks that every page has its front
matter and that every relative link and anchor resolves. Describe what isb
does now, and update the page with the feature.
