# Sandboxes

A sandbox is an isolated machine to run code in: a system container (a whole
Linux machine, starts in seconds) or a VM.

- `sandbox_create`: `spec` is one compose service with `container_name`, as
  an object or YAML (the `isb.yaml` format, topic `cli`). `expires` (default
  24h, at most 30d), `idle_timeout` (default 2h, or `none`). Remote callers'
  specs are refused privileged mode, raw config, host bind mounts outside the
  org's bind roots, and non-loopback ports.
- `sandbox_exec`: `name`, `argv` (a list, never a shell string: use
  `["sh", "-c", "cd app && npm ci"]` for pipes and `&&`), `cwd`, `user`,
  `env`, `stdin`, `timeout` (default 10m). Output is capped at 256 KiB per
  stream.
- `sandbox_list`, `sandbox_extend` (push out the expiry before it passes, or
  the sandbox is deleted), `sandbox_remove`.
- `sandbox_start`, `sandbox_stop`, `instance_restart`: a stopped sandbox keeps
  its state.
- `sandbox_logs`: output of a long-running `restart: always` service, or an
  OCI image's console.
- `sandbox_port_list`, `sandbox_port_add`, `sandbox_port_remove`: proxy
  devices on a running sandbox.
- `sandbox_device_remove`; devices are listed by `instance_get`.
- Files: `instance_file_read` (at most 4 MiB), `instance_file_write` (at most
  2 MiB).

```json
{"spec": {"container_name": "task1", "image": "images:ubuntu/24.04/cloud",
          "type": "vm", "egress": ["pypi.org", "files.pythonhosted.org"],
          "labels": {"owner": "task1"}},
 "expires": "4h"}
```

Images: a local alias (`dev-base`), `images:debian/12`, an OCI image
(`docker:nginx:1.27`, `docker:traefik/whoami`, `ghcr:org/app:tag`) or the
org's own builds, `registry:APP:TAG`. After the prefix a colon is a tag:
`docker:traefik:whoami` is the image `traefik` tagged `whoami`. An OCI
image's `command` is its whole command line, and its `user` must be numeric.

VMs need a VM image (`images:ubuntu/24.04/cloud`), take tens of seconds to
boot, and wait for the incus agent before exec works.

Readiness (`ready`): `running`, `default_route`, `agent` (VMs),
`{user_exists: U}`, `{path_writable: P}`, `{command: [argv]}`.
