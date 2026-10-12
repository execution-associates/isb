---
title: Concepts
description: The ideas isb is built on, from a single sandbox up to orgs on a shared host.
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
- **Sandboxes** made through the daemon (`sandbox_create`) are an org's
  short-lived machines: they expire and are reaped when idle. A long-lived
  machine is an ordinary one-replica stack with a volume for its home. See
  [Sandboxes are short-lived](sandboxes.md#sandboxes-are-short-lived).
- One isb manages **one host**, and every org on it shares the host's
  kernel. Several hosts are several isbs, each connected to the same agent
  over MCP. See [Several hosts](../guides/agents.md#several-hosts).
- **Users, roles and superadmins** decide who may do what
  ([Users, roles and superadmins](access.md)), and
  [the security model](security.md) explains what keeps orgs apart from each
  other and from the host.

```text
host
└── org              incus project + bridge + network ACL + quotas
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
- [Security model](security.md)
- [Users, roles and superadmins](access.md)
