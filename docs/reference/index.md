---
title: Reference
description: Exact, exhaustive descriptions of isb's commands, file format, tools, APIs and settings.
order: 4
---

The reference is where to look up an exact flag, field, tool or endpoint.
The [guides](../guides/index.md) explain how to get something done; these
pages say precisely what each piece does, with its defaults and limits.

| Page | What it covers |
|---|---|
| [CLI](cli.md) | Every `isb` command and flag, global flags, exit codes |
| [isb.yaml](compose.md) | The compose file: every field, interpolation, how `isb up` reconciles |
| [MCP tools](mcp-tools.md) | Every tool `isb serve` offers to agents, people and the web UI, and who may call it |
| [HTTP API](http-api.md) | The daemon's HTTP surfaces: MCP, REST, events, the terminal and SSH websockets, webhooks, the OpenAPI document |
| [Identity API](identity-api.md) | `/api/v1/auth/*`: sign-in flows, passkeys, sessions, CSRF, rate limits, endpoints |
| [Configuration](configuration.md) | Every `isb serve` flag and environment variable, and the other variables isb reads |
| [rpc protocol](rpc.md) | `isb rpc`, the line-delimited JSON protocol the SDKs speak |
| [isb tui](tui.md) | The terminal dashboard: what it shows and its keys |
