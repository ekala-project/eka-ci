# Monitoring & Metrics

Eka CI exposes Prometheus metrics and structured logs. Together they cover build queue
health, cache utilization, GitHub integration, and rebuild detection.

## Prometheus metrics

Metrics are served at `/metrics` on the address configured in `[web]`. Common series:

| Metric | Type | Description |
|---|---|---|
| `eka_ci_build_queue_depth` | gauge | Pending builds per platform queue. |
| `eka_ci_build_duration_seconds` | histogram | End-to-end build wall time. |
| `eka_ci_build_outcome_total` | counter | Builds by outcome (`success`, `failed`, `cancelled`). |
| `eka_ci_graph_cache_hits_total` | counter | LRU cache hits for the dependency graph. |
| `eka_ci_graph_cache_misses_total` | counter | LRU cache misses. |
| `eka_ci_graph_cache_size` | gauge | Current number of nodes in the LRU cache. |
| `eka_ci_webhook_processing_seconds` | histogram | Webhook handler latency. |
| `eka_ci_rebuild_count` | histogram | Rebuilds detected per PR. |
| `eka_ci_change_summary_render_seconds` | histogram | Change-summary render time. |

For deeper guidance on the cache metrics specifically, see
[LRU Cache Tuning](./lru-cache-tuning.md).

### Useful queries

```promql
# Build queue depth, per platform
eka_ci_build_queue_depth

# Cache hit rate over 5 minutes
rate(eka_ci_graph_cache_hits_total[5m])
  / (rate(eka_ci_graph_cache_hits_total[5m])
     + rate(eka_ci_graph_cache_misses_total[5m]))

# 95th percentile webhook latency
histogram_quantile(0.95,
  rate(eka_ci_webhook_processing_seconds_bucket[5m]))
```

## Logging

Logs are emitted via the `tracing` crate as structured records. Verbosity is controlled
through `RUST_LOG`:

```bash
# Set a global level
RUST_LOG=info eka-ci-server

# Per-module filters
RUST_LOG=eka_ci_server::scheduler=debug,eka_ci_server=info eka-ci-server
```

When run under systemd, view logs with:

```bash
journalctl -u eka-ci -f
```

Key log targets:

- `eka_ci_server::scheduler` — build scheduling and queue transitions.
- `eka_ci_server::webhooks` — incoming GitHub events.
- `eka_ci_server::graph` — dependency graph and LRU cache activity.
- `eka_ci_server::change_summary` — change-summary pipeline.
- `eka_ci_server::cache_push` — cache push results and post-build hooks.

## Recommended alerts

A starting set of alerts for production:

- **`eka_ci_build_queue_depth` is high for too long** — pending work is not draining.
- **Webhook 5xx rate non-zero** — GitHub deliveries are being rejected.
- **Cache hit rate < 0.6 sustained** — LRU is undersized; see
  [LRU Cache Tuning](./lru-cache-tuning.md).
- **Change-summary check stuck pending > 10 minutes** — see
  [Change Summaries](./change-summaries.md).

The runbook pages for the LRU cache and change-summary pipeline include more specific
threshold and remediation guidance.
