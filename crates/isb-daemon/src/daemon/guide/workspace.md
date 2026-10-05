# The workspace

An org's workspace is its long-lived machine, with a home directory and an
org token.

Inside it: the token is at `/run/isb/token` (also `$ISB_TOKEN` in login
shells), with `$ISB_URL` (the org's listener), `$ISB_ORG` and
`$ISB_WORKSPACE`. It reaches that org and nothing else: no other org, no
host tools. There is no incus socket: make sandboxes with `sandbox_create`,
not `isb create`, and put heavy or risky work in a sandbox, not in the
workspace itself. The token is never shown by any tool; do not print or copy
it.

Tools: `workspace_get`, `workspace_start`; `workspace_stop`,
`workspace_restart`, `workspace_rebuild` and `workspace_delete` end every
session on it, so without `confirm: true` they only report the live
sessions. `workspace_settings`, `workspace_update`, `workspace_setup_run`,
`workspace_terminals`, `workspace_port_*` (expose a port),
`workspace_image_*`, `workspace_token_rotate`.
