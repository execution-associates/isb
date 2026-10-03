---
title: Concepts
description: The ideas isb is built on, from a single sandbox up to orgs placed on other machines.
order: 2
---

isb has a small number of ideas, and each one builds on the one before. Read
this section when you want to know why isb behaves the way it does, not just
which command to type.

## The model in one page

- A **sandbox** is one incus instance (a system container, or a VM) that isb
  creates from a description and keeps matching it. `isb up` and `isb create`
  work with sandboxes directly, with no daemon. See
  [Sandboxes and reconciling](sandboxes.md).
- A **stack** is a compose file handed to the `isb serve` daemon, which keeps
  it running: replicas, health checks, a load balancer, rolling updates and
  rollbacks. See [Stacks](stacks.md).
- An **org** is the trust boundary. Its people and agents fully administer
  what is in it and reach nothing in any other org. On a host, an org is an
  incus project with its own network and quotas. See [Orgs](orgs.md).
- **Projects, environments and apps** are the layer over stacks, the way
  Dokploy presents it: you say what to run (an image, a git repository, a
  database) and isb keeps the deployments, logs and webhooks. See
  [Projects, environments and apps](apps.md).
- A **workspace** is an org's long-lived machine where its people and agents
  work, and it is itself an actor in the org. **Sandboxes** made through the
  daemon are its short-lived companions. See
  [Workspaces and sandboxes](workspaces.md).
- **Placement** decides where an org runs: on this host, on another server,
  or in a dedicated VM with its own kernel. See [Placement](placement.md).
- **Users, roles and superadmins** decide who may do what
  ([Users, roles and superadmins](access.md)), and
  [the security model](security.md) explains what keeps orgs apart from each
  other and from the host.

```text
host (or a server, or a dedicated VM)
└── org              incus project + bridge + network ACL + quotas
    ├── workspace    the org's machine, with a home and an org token
    ├── sandboxes    short-lived instances, made by people or agents
    └── project
        └── environment   one stack, <project>-<env>
            └── app       one service of that stack, with replicas
```

## Pages

- [Sandboxes and reconciling](sandboxes.md)
- [Orgs](orgs.md)
- [Projects, environments and apps](apps.md)
- [Stacks](stacks.md)
- [Workspaces and sandboxes](workspaces.md)
- [Placement](placement.md)
- [Security model](security.md)
- [Users, roles and superadmins](access.md)
