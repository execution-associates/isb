# Look inside and act on what runs

The org's `kubectl`. An instance is any running container or VM: an app or
stack replica, a database, a sandbox, the workspace.

- `instance_list`: kind, app, slot, health, in rotation, IP, restarts, CPU,
  memory. Filter by `app`, `kind`.
- `instance_get`: one in full: env names (never values), volumes, devices,
  domains, history.
- Exec: `app_exec` (`name`, `argv`, `replica`), `stack_exec` (`name`,
  `service`, `argv`), `instance_exec` (any instance by name). Each takes
  `stdin`, `cwd`, `user`, `env`, `timeout` (default 60s, at most 15m); each
  stream is capped at 1 MiB and `truncated`/`timed_out` say so.
- Logs and state: `app_logs` (`tail`, `since`, `replica`), `stack_logs`,
  `app_top`, `app_events`, `events`, `overview`, `metrics_query`,
  `history_query`.
- Act: `app_restart` (rolling, `wait`), `app_scale`, `stack_scale`,
  `instance_restart`.
- Files: `instance_file_read` (at most 4 MiB), `instance_file_write` (at most
  2 MiB; never `/run/isb`, `/run/secrets`, `/etc/isb`, or a file isb delivers
  from a secret).

Exec, restarts and files need a member: a viewer or a `read`/`deploy` token
is refused. There is no port-forward: `curl` through `app_exec`.
