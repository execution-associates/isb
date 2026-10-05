---
name: wrap
description: "Wrap up a finished feature in an isb worktree: bring main in, verify in a sandbox, bump the version, land it through a PR, tag the release, wait for every registry, upgrade titan's isb without rolling a service, refresh the installed isb skill, then close this agent's own herdr pane. Use when the user says \"wrap\", \"/wrap\", \"wrap it up\", \"ship it\", or asks to finalize/land/release a completed isb feature."
---

# wrap: land a finished isb feature, release it, upgrade titan, close out

Run this from **inside an agent working in an isb git worktree** whose branch
holds a *completed* feature. It takes the branch to a published release
running on titan, then closes the agent. The last step kills this agent's
terminal, so everything before it must succeed first.

End to end: **preflight → main into the branch → verify in a sandbox → bump →
PR, CI, squash-merge → main's CI → tag → wait for the release and registries →
upgrade titan → refresh the installed skill → close own herdr pane.**

`$ARGUMENTS` may name the bump: `major`, `minor`, `patch` or `X.Y.Z`. Default
`patch`; a feature that adds user-facing surface (a tool, a flag, a compose
key) is `minor`, so say so if the default looks wrong.

**Each step is checked.** If one fails, stop and report. A half-finished wrap
that still closed the agent is the worst outcome.

## What is different from other repos

- **main publishes itself.** A global `post-commit`/`post-merge` hook pushes
  `main` the instant it moves, so nothing is ever committed or merged on local
  `main` here. Everything lands through a PR, squash-merged on GitHub, which
  is also how this repo's history reads (`isb X.Y.Z: <summary> (#N)`).
- **The bump rides in the feature's PR.** The tag must equal Cargo.toml's
  version (release.yml refuses otherwise), and the squash commit is what gets
  tagged.
- **Builds never run on the host.** `cargo`, `uv`, `bun` run in an isb sandbox
  that binds only this worktree. The incus socket never goes into it.
- **titan's isb is shared and live.** It runs every org's apps (fiftytwolabs,
  norm, ticket500, exa, ...). Other agents also release isb and touch titan's
  install, so the upgrade is coordinated, and it must roll nothing.

## 0. Preflight

```bash
feature=$(git branch --show-current)   # abort if empty or main
git status --porcelain                  # feature-shaped changes: commit them; surprising ones: ask
H=$(git config --get core.hooksPath); ls "${H:-.git/hooks}"/post-merge 2>/dev/null && echo "main auto-pushes"
```

**Look for other agents on isb or titan before anything is merged.** List the
sessions (Claude Code's `ListAgents`, or `herdr agent list`) and read the ones
whose names or worktrees say isb, a release, or titan's install. If one is
mid-release or about to restart `isb.service`, message it (`SendMessage`),
agree an order (who takes which version), and wait for its go. Two releases
racing for one version number, or two restarts of titan's daemon, are what
this prevents.

## 1. Bring main INTO the feature branch

```bash
git fetch origin
git merge origin/main
```

Resolve conflicts here, where nothing is published. If one can't be resolved
with confidence, `git merge --abort` and report. A clean merge can still be
semantically broken (a helper deleted on main that this branch calls), which
is what step 2 catches.

## 2. Verify in a sandbox (what CI runs)

The sandbox binds this worktree and nothing else: never `$HOME`, `~/.ssh`,
`~/.config`. Label it so a stray is findable.

```bash
WT=$PWD; SB=isb-wrap-$(basename "$WT" | tr -c 'a-z0-9-\n' '-' | cut -c1-40)
D=$(mktemp -d)   # or this session's scratchpad
cat >"$D/isb.yaml" <<EOF
services:
  build:
    container_name: $SB
    image: dev-base
    cpus: 16
    mem_limit: 48g
    idmap: auto
    labels: {owner: $SB}
    volumes:
      - $WT:/home/dev/isb
    ready: [running, default_route, {user_exists: dev}]
    user: dev
    working_dir: /home/dev/isb
    exec:
      env: {PATH: "/home/dev/.local/share/mise/shims:/home/dev/.cargo/bin:/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin", CARGO_TARGET_DIR: /home/dev/.cache/isb-target}
EOF
( cd "$D" && isb up -d && isb exec build -- sh -c 'mise use -g rust@stable >/dev/null && rustup component add clippy rustfmt >/dev/null' )
( cd "$D" && isb exec build -- sh -c 'cargo fmt --all && scripts/check.sh' )
```

`scripts/check.sh` runs the docs links, the ratchet, fmt, clippy `-D
warnings`, the tests and the docs build. Formatting runs first because fmt
can grow a file past its ratchet budget.

When the branch touched a tool's schema, the SDKs' generated types
(`sdk/python/src/isb/_spec.py`, `sdk/typescript/src/spec.ts`) must be
regenerated from the new binary; CI's Python SDK job fails when they are
stale:

```bash
( cd "$D" && isb exec build -- sh -c 'mise use -g uv bun >/dev/null && cargo build --locked && export ISB_BIN=/home/dev/.cache/isb-target/debug/isb \
  && (cd sdk/python && uv run python scripts/gen_types.py) && (cd sdk/typescript && bun install --frozen-lockfile && bun run gen-types)' )
git status --short sdk   # commit what changed
```

When it touched `web/openapi.json`, also `bun run gen:api` in `web/`. **Tests that need
incusd** are compiled in the sandbox (`cargo test --no-run`) and the binaries
run on the host; never mount the incus socket into the sandbox.

Keep the sandbox until the PR is green (step 4 may need it again); step 9
deletes it.

## 3. Bump

```bash
( cd "$D" && isb exec build -- scripts/bump.sh "${ARGUMENTS:-patch}" )
VER=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
git commit -am "isb $VER"
```

`scripts/bump.sh` edits every place the version is written (the crates and
their Cargo.lock entries, both SDKs, their lockfiles and tests, the RPC
reference) and fails if the old version is left in any of them. It runs on
the host safely (sed only), but the sandbox is where the tree is checked.
Rerun `scripts/check.sh --quick` if anything other than versions changed.

## 4. PR, CI, squash-merge

```bash
git push -u origin "$feature"
gh pr create --fill   # or `gh pr view` if one exists; title: "isb $VER: <one-line summary>"
gh pr checks --watch --interval 30
```

Fix anything red on the branch (in the sandbox) and push again. A known flake
(macOS x86_64 `connection_cap_and_shutdown`, EINVAL) in code the branch did
not touch: rerun that job once with `gh run rerun <id> --failed`; a second
failure is real.

Before merging, **`git fetch origin` and check main didn't move.** If it did,
go back to step 1: another agent's release landed and the version may now be
taken.

```bash
gh pr merge --squash --subject "isb $VER: <summary> (#N)"
SHA=$(gh pr view --json mergeCommit -q .mergeCommit.oid)
```

## 5. Wait for main's CI on the merge commit

```bash
gh run list --branch main --commit "$SHA" --json name,status,conclusion
```

Wait for every run (CI, Build health, Python SDK, SDK (TypeScript), Release
binaries) to complete green. Release binaries builds the four release
tarballs (Linux and macOS, x86_64 and aarch64) for this commit and keeps them
as artifacts for 30 days; it is the long pole, about 5 minutes. A red main is
fixed forward with a new PR, never by tagging anyway.

## 6. Tag

Tag as soon as step 5 is green:

```bash
git fetch origin
git tag -a "v$VER" "$SHA" -m "isb $VER"
git push origin "v$VER"
```

The tag push is the only thing that starts a release, and nothing pushes it
for you. It promotes what main built and checked rather than redoing it:
each tag workflow first checks that CI passed on main for `$SHA` and refuses
to publish otherwise; Release then signs and publishes Release binaries'
tarballs for `$SHA`, and the SDK workflows skip their `check` job when they
already passed on main. If main has no release binaries for `$SHA` (that run
failed or its artifacts expired), Release builds them itself, which adds
about 10 minutes.

## 7. Wait for the release and the registries

```bash
gh run list --commit "$SHA" --json name,status,conclusion   # Release, Python SDK, SDK (TypeScript)
gh release view "v$VER" --json assets -q '.assets[].name'   # tarballs, SHA256SUMS, SHA256SUMS.sig, install.sh
```

All three workflows must be green, usually within about 6 minutes of the
tag. Release signs SHA256SUMS (an unsigned
release can't be installed by `isb update`), publishes the crates in
dependency order, and its `install` job runs the published install.sh on
Linux and macOS. Each publish step skips a version the registry already has,
so a failed run is re-run from the Actions page, not re-tagged. A crate
version can take a few minutes to show on crates.io, and an npm one up to ~10.

## 8. Upgrade titan's isb, rolling nothing

titan runs `isb serve` as the user unit `isb.service` on `127.0.0.1:8192`,
from `~/.local/bin/isb`.

**Announce first.** Tell any agent working on titan's orgs (step 0's list)
that the daemon is about to restart, and wait for an OK from one that asked
for warning.

**Record the before state**, so "nothing rolled" is a fact, not a hope:

```bash
B=$(mktemp -d)
incus list --all-projects --format json </dev/null \
  | jq -r '.[] | select(.config["user.isb.rev"]) | [.project, .name, .status, .config["user.isb.rev"]] | @tsv' \
  | sort >"$B/before.tsv"
```

**Check the new version computes the same revisions.** A revision that
changes between versions (a new field folded into the hash) means every
service with it rolls on restart. `crates/isb-core/examples/revisions.rs`
prints what this build computes from the daemon's state; build it in the
sandbox, run it on the host (it only reads), and compare with the live
labels:

```bash
( cd "$D" && isb exec build -- cargo build -p isb-core --example revisions --locked )
incus file pull "$SB/home/dev/.cache/isb-target/debug/examples/revisions" "$B/revisions" </dev/null
"$B/revisions" ~/.local/state/isb/orgs | awk '{print $1"/"$2"/"$3, $4}' | sort >"$B/expected"
incus list --all-projects --format json </dev/null \
  | jq -r '.[] | select(.config["user.isb.rev"]) | "\(.project | ltrimstr("isb-"))/\(.config["user.isb.stack"])/\(.config["user.isb.service"]) \(.config["user.isb.rev"])"' \
  | sort -u >"$B/live"
join "$B/expected" "$B/live" | awk '$2 != $3'   # expect nothing
```

Any line printed is a service the restart would roll. Stop and plan that
roll with the user (which orgs, when, one at a time with health checks).

**Upgrade and restart:**

```bash
~/.local/bin/isb update "$VER"
systemctl --user restart isb
~/.local/bin/isb --version                       # isb $VER
systemctl --user is-active isb
```

**Verify after ~30s:**

```bash
incus list --all-projects --format json </dev/null \
  | jq -r '.[] | select(.config["user.isb.rev"]) | [.project, .name, .status, .config["user.isb.rev"]] | @tsv' \
  | sort >"$B/after.tsv"
diff "$B/before.tsv" "$B/after.tsv" && echo "nothing rolled"
journalctl --user -u isb --since "-2min" | grep -iE "error|panic" | head
```

Same instance names, same status, same revisions. Then spot-check a few
public routes per org return what they did before (the app's health URL, not
just a 200 from the tunnel). Report the before/after evidence to the agents
you warned.

**Never** `isb app deploy` or `stack redeploy` a live app as part of a wrap:
an app deploy always replaces its instances, and a database gets ~20s of
stop-first downtime.

## 9. Refresh the installed isb skill, clean up

The isb skill agents load is a copy of the repo at a tag:
`~/.agents/skills/isb` (which `~/.claude/skills/isb` links to) and
`~/.agents/skills-flat/isb`.

```bash
for d in ~/.agents/skills/isb ~/.agents/skills-flat/isb; do
  find "$d" -mindepth 1 -delete
  git archive "v$VER" | tar -x -C "$d" --exclude=.github --exclude=.gitignore
done
TAGGED=$(git rev-parse "v$VER^{commit}")
jq --arg h "$TAGGED" --arg t "$(date -u +%FT%T.000Z)" \
  '.skills.isb.skillFolderHash = $h | .skills.isb.updatedAt = $t' \
  ~/.agents/.skill-lock.json >"$B/lock" && mv "$B/lock" ~/.agents/.skill-lock.json
```

Then delete what this wrap made:

```bash
( cd "$D" && isb down ) && isb ls --label "owner=$SB"   # expect nothing
rm -rf "$D" "$B"
```

## 10. Close this agent, LAST

Print the summary first: merged `<feature>` as `#N`, released `v$VER`
(crates.io, PyPI, npm), titan on `v$VER` with nothing rolled, installed skill
refreshed. Then close **this agent's own** herdr pane:

```bash
herdr pane close "$HERDR_PANE_ID"   # confirm with `herdr pane current` if unset
```

Never close a pane that isn't this agent's. Only reach here once steps 0–9
succeeded; the connection dropping afterwards is success.
