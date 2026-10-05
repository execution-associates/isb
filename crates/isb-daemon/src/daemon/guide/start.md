# isb: start here

isb runs incus system containers and VMs. Everything lives in an **org**
(an incus project with its own network): sandboxes, apps and stacks,
databases, jobs, secrets, domains and the org's workspace.

## Where are you?

| You hold | You are | Use |
|---|---|---|
| this MCP at `/orgs/ORG/mcp`, or an `isb_tok_` token | a user (or an agent for one) in that org | these tools; `org` is fixed to ORG |
| `/run/isb/token` and `$ISB_URL` | the org's workspace | the org MCP at `$ISB_URL/orgs/$ISB_ORG/mcp`, or the `isb` CLI (it uses `$ISB_URL` and `$ISB_TOKEN`) |
| the unbound `/mcp` as superadmin | the host's operator, remotely | every org and the host tools. It is root on the host: act as narrowly as an org token would |
| the incus socket on the host | the host's operator | the `isb` CLI (topic `cli`) |

`whoami` says who you are and what you may do. A call your role or token
scopes do not allow is refused with `forbidden`: a viewer, or a token scoped
`read` or `deploy`, cannot exec, write files or read secrets.

Over REST each tool is `POST /orgs/ORG/api/v1/tools/TOOL` with the arguments
as the JSON body and `Authorization: Bearer TOKEN`.

## Topics

- `safety`: the rules. Read before running code you did not write.
- `sandboxes`: an isolated machine to run code in, its ports, logs, volumes.
- `apps`: projects, apps, stacks, builds, templates, images, domains.
- `inspect`: look inside and act on what runs (like kubectl).
- `data`: databases, backups, volumes and snapshots, jobs, secrets.
- `workspace`: the org's long-lived machine.
- `admin`: orgs, members, invitations, tokens, SSH keys, users, audit.
- `cli`: the `isb` CLI, `isb.yaml` and `isb up` on a host; the SDKs.
