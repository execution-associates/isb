---
title: Metrics history
description: A month of CPU, memory, network and disk numbers for every instance of every org, kept in tiers and queried with metrics_query.
order: 6
---

To see whether an app's memory creeps up over a week, or which replica ate
the CPU last night, you need more than the live numbers. `isb serve` samples
every instance every 2 s (the live numbers `overview` and `stack_status`
show) and keeps a month of history per org, for the web UI's Monitoring tab
and anything else that asks. It survives daemon restarts and needs no
separate metrics stack.

## What is kept

Per instance of an isb org (instances in other incus projects are not kept):

| Metric | Unit | From |
|---|---|---|
| `cpu` | percent of one core (four busy cores read 400) | incus instance state |
| `memory` | bytes in use | incus instance state |
| `net_rx`, `net_tx` | bytes per second, every interface but loopback | the interface counters |
| `disk_read`, `disk_write` | bytes per second, every disk | incus `/1.0/metrics`, read every 10 s |

Rates come from counters; a counter that goes backwards (a restart) gives no
rate for that sample rather than a negative one. Stopped instances are not
recorded.

## Tiers

| Step | Kept |
|---|---|
| 10 s | 24 hours |
| 1 min | 7 days |
| 10 min | 30 days |

Each 10 s bucket (the average of the samples in it) is written as it closes;
once a minute the coarser tiers are rolled up from the one below and older
rows are deleted. The history lives in `<state>/orgs/<org>/metrics.db`
(SQLite, WAL), one database per org.

Disk use is bounded by the number of instances: about 23 000 rows per
instance at the steady state. Measured with 20 instances over 31 simulated
days (`cargo test --release --lib measure_disk_use -- --ignored`): 460 620
rows in 20.5 MiB; writing one 10 s bucket of 20 instances takes 1.7 ms, a
roll-up and retention pass 0.2 s (once a minute), and a 24 h query summing a
service over them 0.2 s. Memory is one open bucket per running instance;
samples reach the writer through a queue of 8 and are dropped (never block
the sampler) when the disk falls behind.

## Querying

The `metrics_query` tool (over REST: `POST /api/v1/tools/metrics_query`)
answers series ready for a chart:

```json
{"org": "acme", "app": "web", "metric": "cpu", "range": "6h", "aggregate": "sum"}
```

| Argument | |
|---|---|
| `metric` | `cpu`, `memory`, `net_rx`, `net_tx`, `disk_read`, `disk_write` |
| `app` | an app: its service in its project environment's stack |
| `stack`, `service` | a stack (all its services), or one service of it |
| `instance` | one instance |
| `range` | how far back from `to` (`1h` default, `24h`, `7d`, `30d`) |
| `from`, `to` | unix seconds instead (`to` defaults to now) |
| `step` | seconds per point; widened to the step of the tier that still holds `from`, and so that there are at most 2000 points |
| `aggregate` | `sum`, `avg`, `max` or `min` over the instances, bucket by bucket (a service's replicas); without it, one series per instance |

```json
{"metric": "cpu", "from": 1791000000, "to": 1791021600, "step": 60, "tier_step": 10,
 "series": [{"name": "shop-production/web", "stack": "shop-production", "service": "web",
             "replicas": [2, 2, ...], "points": [[1791000000, 12.5], ...]}]}
```

`points` are `[bucket start, value]`, oldest first; a bucket with no samples
is absent (a gap), not zero. `replicas` (aggregates only) counts the
instances with a value in each bucket. A replaced replica is a new instance,
so a per-instance query over a rolling update shows the old series ending and
the new one starting; an aggregate over the service runs straight through.
The coarser tiers lag their roll-up by a minute or two, so a query reading
them takes its newest buckets from the tier below.

With an API token, from anywhere that reaches the daemon:

```sh
curl -s -H "Authorization: Bearer $ISB_TOKEN" -H 'Content-Type: application/json' \
  -d '{"app": "web", "metric": "memory", "range": "24h", "aggregate": "max"}' \
  "$ISB_URL/orgs/acme/api/v1/tools/metrics_query"
```

The org is the boundary: members of an org query its instances and no other
org's ([Users, roles and superadmins](../concepts/access.md)).

## In the web UI

An app's **Monitoring** tab charts this history over 1 hour, 24 hours, 7 days
or 30 days ([The web UI](../getting-started/web-ui.md)); the workspace's
Resources tab and the org overview show live CPU and memory with a sparkline.
