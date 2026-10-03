# Developing isb

Build, test and run isb inside a sandbox, not on a host that holds
credentials: a build script or a dependency runs with whatever the shell it
starts in can reach. The incus socket is root-equivalent, so the integration
tests are compiled in the sandbox (`cargo test --no-run`) and run on the host
(see the README).

## The checks

```sh
scripts/check.sh           # ratchet, fmt, clippy -D warnings, unit tests
scripts/check.sh --quick   # the same without the tests
```

CI runs the same steps (`.github/workflows/ci.yml` and `build-health.yml`).

## The legibility ratchet

New code may not be harder to read than the rule, and old code may get
better but never worse.

- **Clippy thresholds** (`clippy.toml`, lints enabled in `Cargo.toml`
  `[lints.clippy]`, denied in CI):

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
  `#[allow(clippy::<lint>, reason = "predates the lint ratchet; ...")]`.
- **Exemption budget**: `scripts/ratchet.sh` counts how often each of those
  lints is named in the source and fails when a count exceeds its budget in
  `scripts/ratchet.txt`, so a new `#[allow]` cannot slip in unnoticed.
- **File size**: the same script fails any `.rs` file over 1000 lines, except
  the files listed in `scripts/ratchet.txt`, each capped at its listed size.

Both budgets go down only. After splitting a long function or file, run
`scripts/ratchet.sh --update` to lower them (it never raises one) and commit
the new `scripts/ratchet.txt`. Raising a budget is a hand edit a reviewer
sees; splitting is almost always the better fix. When you change a function
that carries an exemption, split it and drop the `#[allow]`.

## Compile time

`scripts/build-times.sh` measures the edit-compile loop: each of `cargo
check`, a debug build, `cargo test --no-run`, `cargo clippy` and a release
musl build, after a one-line edit in a leaf module and in a widely used one.

```sh
scripts/build-times.sh                 # check build test clippy
scripts/build-times.sh release         # minutes per cell
RUNS=3 scripts/build-times.sh check    # best of three
```

To see where a full build spends its time, `cargo build --timings` writes
`target/cargo-timings/cargo-timing.html` (CI's `build timings` job uploads
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
