# Admin: orgs, people, tokens, audit

- Orgs: `org_list`, `org_get`, `org_create`, `org_update`, `org_delete`,
  `org_nesting`. An org's domain allowlist and ingress are set by a platform
  admin or superadmin.
- Members: `member_list`, `member_update` (role: owner, admin, member,
  viewer), `member_remove`; `invitation_create`, `invitation_list`,
  `invitation_revoke`.
- Tokens: `token_create` (`scopes`: `read`, `deploy`, `admin`, or `tool:GLOB`; `expires`),
  `token_list`, `token_revoke`; sessions: `session_list`, `session_revoke`.
  The token value is shown once.
- SSH keys for `isb ssh-proxy`: `ssh_key_add`, `ssh_key_list`,
  `ssh_key_remove`; `ssh_host_keys` pins instances' host keys.
- Agents acting for a user: `agent_identity_*`.
- Users (platform admins): `user_list`, `user_update`. Creating users,
  setting someone's password, and minting tokens or adding SSH keys for
  someone are on the host only.
- What happened: `audit_list` and `audit_verify` (owners and admins),
  `history_query`, `events`.
- Alerts: `notification_channel_*`, `notification_test`; uptime checks:
  `monitor_*`.
- Servers the control plane places orgs on (superadmin): `server_*`; live
  CPU, memory, disk and network of the host or one server: `host_monitor`.

Superadmin identities, superadmin tokens and an org's bind roots are set on
the host only too (`isb superadmin`, `isb token create --superadmin`,
`isb org create --bind-root`): they are trust roots, never granted over HTTP.
