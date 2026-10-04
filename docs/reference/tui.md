---
title: isb tui
description: The terminal dashboard of stacks and sandboxes: what it shows, where its data comes from, and its keys.
order: 8
---

A live view of the host, its stacks and its sandboxes, and the everyday
operations on them, in the terminal: watch a rollout slot by slot, read
logs, open a shell, scale, deploy and roll back without leaving it. It needs
a terminal; scripts and agents use `isb stack ps` or the
[tools](mcp-tools.md) instead.

```console
$ isb tui
```

```
 isb · myhost                                                           isb 1.0.2 · serve ●
 stacks 1   replicas 4/4   sandboxes 9 (4 up)   cpu ▂▃▅▃▂▁▂▃ 12%   mem ▕█▋░░░░░░▏ 50G/252G
──────────────────────────────────────────────────────────────────────────────────────────
╭ overview ─────────────╮┌ e2e ─────────────────────────────────────────────────────────┐
│ STACKS                ││ ◐ updating   deployed 9s ago by local(uid 1000)               │
│ ◐ e2e         ↻ 3/4   ││ SERVICE   STATE      REPLICAS  PORT            TRAFFIC        │
│                       ││ ● cache   converged  ● 1/1     :16379 → 6379   ▁▁▁▁▁▁   0/s  │
│ SANDBOXES             ││ ◐ web     updating   ●●● 3/3   :18080 → 8000   ▁▃▅▇█▆  48/s  │
│ ● build-box           ││                                                               │
│ ● agent-k7q2     mcp  ││  web rolling out ──────────────────────────────────────────── │
│ ○ old-dev         vm  ││ f739806f → 8a17158c   start-first   ▕████▍░░░░░░▏ 1/3         │
│                       ││ slot 1  f739806f  ○ retired   ━━━▶  8a17158c  ● serving       │
│                       ││ slot 2  f739806f  ● serving   ━━━▶  8a17158c  ◐ creating      │
│                       ││ slot 3  f739806f  ● serving     ·    8a17158c  ◌ waiting       │
╰───────────────────────╯└───────────────────────────────────────────────────────────────┘
 events ───────────────────────────────────────────────────────────────────────────────────
 02:12:45 · e2e/web   slot 1: e2e-web-1-63c0 is serving; retiring e2e-web-1-07f2
 ↑↓ move  ⏎ open  l logs  e shell  s scale  r redeploy  b rollback  x remove  ? help
```

## Where the data comes from

With the `isb serve` daemon running (its socket at `$ISB_SERVE_SOCKET`, else
`$XDG_RUNTIME_DIR/isb/serve.sock`), the dashboard reads the daemon's
`overview` tool every second and follows its `events` tool. A web UI would
read the same two tools, so the two always show the same thing. Without the
daemon it reads incus directly: sandboxes only, with no stacks and no event
feed, and the header says `direct · read-only`.

Shells, sandbox logs and starting, stopping or removing a sandbox go to incus
directly either way, so they need the same incus access as `isb exec`.

## Reading it

| Glyph | Means |
|---|---|
| `●` | healthy, converged, running, serving |
| `◐` | updating, probing, creating, draining |
| `◌` | starting, waiting |
| `◫` | paused |
| `✖` | failing, unhealthy |
| `○` | stopped, retired |

Colour adds to the glyphs and never replaces them, so `NO_COLOR=1` loses
nothing. Sparklines show the last minute or so: CPU as a percentage of one
core (four busy cores read 400%), traffic as new connections per second
through the balancer. `LB ⇄` marks a replica the balancer sends traffic to.

The header's `disk` is the incus storage pools' used and total space (pools
on one filesystem, such as several `dir` pools, count once), re-read every
30 seconds; it needs a terminal at least 130 columns wide. A replica's `DISK`
and a sandbox's `disk` are its root disk's usage, which only some storage
drivers report (ZFS, Btrfs, LVM); on a `dir` pool they show `-` and are left
out.

Columns give way as the terminal narrows: image, revision and traffic in the
service table, then IP, rotation and restarts, then disk, in the replica
table.

## Keys

| Key | Does |
|---|---|
| `↑` `↓` `j` `k`, `g` `G` | move |
| `⏎` `tab` `→` | into a stack's services, then its replicas |
| `esc` `←` `⇧tab` | back out (and `esc` clears a filter) |
| `l` | logs: the service's replicas interleaved by time, or one replica's when a replica is selected, or a sandbox's journal or console. `f` follows, `w` wraps, `1`-`9` picks a replica, `a` shows all |
| `e` | a shell in the selected replica or sandbox (root, bash if it has one); exit to come back |
| `s` | scale the service |
| `r` | redeploy the service: fresh replicas, rolling |
| `b` | roll the stack back to its previous deployment |
| `x` | remove the stack or sandbox (type its name to confirm) |
| `t` | start or stop a sandbox |
| `d`, `:deploy [FILE] [NAME]` | deploy a compose file: shows each service's change, then asks |
| `/` | filter stacks and sandboxes by name |
| `:` | command palette (`deploy`, `stacks`, `sandboxes`, `filter`, `refresh`, `help`, `quit`) |
| `R` | refresh now |
| `?` | help |
| `q`, `ctrl-c` | quit |

`:deploy` reads the file where `isb tui` was started, exactly as
`isb stack deploy` would (`.env`, `${VAR}`, secrets from the environment), and
asks the daemon for a plan before applying anything.
