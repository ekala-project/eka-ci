# Change Summary Operational Runbook

## Table of Contents

1. [Overview](#overview)
2. [Per-Repo Configuration](#per-repo-configuration)
3. [Metrics](#metrics)
4. [Endpoints](#endpoints)
5. [GitHub Check Posting](#github-check-posting)
6. [Truncation Strategy](#truncation-strategy)
7. [Cache](#cache)
8. [Troubleshooting](#troubleshooting)
9. [Alerts](#alerts)

---

## Overview

The change-summary pipeline computes a per-PR view that combines:

- **Package changes** (A1): structured diff between head and base jobsets — Added / Removed / VersionBump / LicenseChange / MaintainerChange / RebuildOnly.
- **Rebuild impact** (A2): per-system rebuild counts plus per-package "blast radius" (count of transitive dependents) across the in-memory build graph.

Outputs:

- `GET /v1/commits/{sha}/package-changes` — JSON, structured diff only.
- `GET /v1/commits/{sha}/rebuild-impact` — JSON, impact only.
- `GET /v1/commits/{sha}/change-summary` — JSON, combined + pre-rendered markdown.
- `GET /v1/commits/{sha}/change-summary.md` — `text/markdown`, ready to paste.
- A single GitHub check run per PR head (`EkaCI: Change Summary`), idempotently created/patched on a 5-minute debounce after each jobset evaluation.

---

## Per-Repo Configuration

Behaviour can be tuned per repository via `.ekaci/config.json`. Both blocks are optional; absent ⇒ engine defaults.

```json
{
  "package_change_summary": {
    "enabled": true,
    "max_packages_listed": 100,
    "include_rebuild_only": false
  },
  "rebuild_impact": {
    "enabled": true,
    "max_top_blast_radius": 5,
    "compute_full_blast_radius": false
  }
}
```

**`package_change_summary`**

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Hides the package-changes section of the check when `false`. |
| `max_packages_listed` | `100` | Soft cap on table rows before the renderer collapses to counts-only. The web endpoint clamps user-supplied `max_packages_listed` query params to ×10 this value. |
| `include_rebuild_only` | `false` | When `true`, `RebuildOnly` rows render alongside Added/Removed/Bumped. Counts are still surfaced in the rebuild-only summary line regardless. |

**`rebuild_impact`**

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Hides the blast-radius section of the check when `false`. |
| `max_top_blast_radius` | `5` | Number of top-rebuild packages reported. The web endpoint clamps user-supplied `max_top_blast_radius` query params to ×10 this value. |
| `compute_full_blast_radius` | `false` | When `true`, walks the full transitive dependent set (expensive on large jobsets — use sparingly). The default mode reports per-seed direct rebuild counts. |

Schema is parsed by `CIConfig` in `backend/server/src/ci/config.rs`. Partial blocks are accepted; missing inner fields fall back to the defaults shown above.

---

## Metrics

All metrics are exposed at `GET /v1/metrics` with the namespace `eka_ci_`.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `eka_ci_change_summary_total_duration_seconds` | Histogram | `phase` | Wall-clock per phase: `classify`, `impact`, `render`, `end_to_end`. |
| `eka_ci_change_summary_cache_hits_total` | Counter | — | `RebuildImpactCache` lookups served from SQLite. |
| `eka_ci_change_summary_cache_misses_total` | Counter | — | `RebuildImpactCache` lookups that fell through to a cold compute. |
| `eka_ci_change_summary_metadata_unavailable_total` | Counter | — | Calls where head-side `pname`/`version`/`license`/`maintainers` were entirely missing. Indicates the eval pipeline did not populate package metadata. |
| `eka_ci_change_summary_truncated_total` | Counter | `level` | Truncation events by drop level: `columns` (one of maintainers/license/rebuild-only dropped) or `summary` (table collapsed to counts-only). |
| `eka_ci_rebuild_impact_traversal_duration_seconds` | Histogram | `system` | Per-system blast-radius traversal duration. |
| `eka_ci_rebuild_impact_seeds_total` | Histogram | — | Per-call distribution of changed-drv seed count fed into the BFS. |

### Useful queries

Cache hit ratio (target: > 0.8 in steady state for re-rendered PRs):

```promql
rate(eka_ci_change_summary_cache_hits_total[5m])
  /
ignoring() (
  rate(eka_ci_change_summary_cache_hits_total[5m])
  + rate(eka_ci_change_summary_cache_misses_total[5m])
)
```

End-to-end p95:

```promql
histogram_quantile(0.95,
  sum by (le) (rate(eka_ci_change_summary_total_duration_seconds_bucket{phase="end_to_end"}[5m]))
)
```

Truncation rate (target: < 5% of renders):

```promql
sum(rate(eka_ci_change_summary_truncated_total[15m]))
  /
sum(rate(eka_ci_change_summary_total_duration_seconds_count{phase="end_to_end"}[15m]))
```

---

## Endpoints

| Path | Auth | Notes |
|---|---|---|
| `GET /v1/commits/{sha}/package-changes` | required | Returns full structured diff; never truncated by the orchestrator. Query: `base_sha`, `job`, `max_packages_listed`. |
| `GET /v1/commits/{sha}/rebuild-impact` | required | Read-through `RebuildImpactCache`. Query: `base_sha`, `job`, `max_top_blast_radius`. |
| `GET /v1/commits/{sha}/change-summary` | required | Combined; includes pre-rendered markdown. |
| `GET /v1/commits/{sha}/change-summary.md` | public | Returns the same markdown that posts to the GitHub check. Public per design §10.1 — same data is visible on the PR check tab. |

`max_packages_listed` and `max_top_blast_radius` are clamped to ×10 their defaults to keep payload sizes predictable.

---

## GitHub Check Posting

- One check run per PR head SHA, titled **EkaCI: Change Summary**.
- Posted with `status=Completed`, `conclusion=Neutral` (informational; does not gate merge).
- 5-minute debounce after the last `CreateJobSet` for a head SHA so all jobsets contribute to a single aggregated render.
- Idempotent: subsequent renders for the same head PATCH the same check run id.
- Defense-in-depth: a 65,500-byte sender-side cap protects against GitHub's 65,535-char `output.summary` ceiling. Hits append a `_…truncated by sender safety net_` footer; this is rare in practice (the markdown renderer's 60,000-byte soft limit fires first).

---

## Truncation Strategy

The renderer drops content in priority order until the markdown fits under 60,000 bytes:

1. Drop **maintainer** rows from the package table.
2. Drop **license** rows.
3. Drop the **rebuild-only** count line.
4. Collapse the entire change table to a counts-only summary.

Every step that fires increments `eka_ci_change_summary_truncated_total` (`level=columns` for steps 1-3, `level=summary` for step 4).

---

## Cache

The `RebuildImpactCache` SQLite table memoises rebuild-impact responses keyed by `(head_sha, base_sha, job)`.

- Pruned on startup: rows older than 7 days are dropped (`DEFAULT_CACHE_TTL_DAYS`).
- Cache write failures are logged at WARN; the freshly-computed answer is still returned (just unmemoised).
- 404s (head jobset missing) are **not** cached.

To force recompute of a specific entry:

```sql
DELETE FROM RebuildImpactCache WHERE head_sha = ? AND base_sha = ? AND job = ?;
```

---

## Troubleshooting

### "No change summary check appearing on a PR"

1. Check that the PR target jobset finished evaluating (`Job` rows for the head SHA exist).
2. Confirm the GitHub App has `checks: write` permission for the repo.
3. The 5-minute debounce means the check appears at least 5 minutes after the last jobset. Verify by waiting or by checking the `change_summary_pending` log line in the GitHub service.
4. If a base SHA is missing from the PR (rare; happens on detached PR heads), the change-summary check is skipped — this is intentional.

### "Rendered summary is truncated more than expected"

- Inspect `eka_ci_change_summary_truncated_total` rates. A spike usually correlates with a large fan-out PR (touches many packages).
- Per design §11.1, the per-repo `max_packages_listed` and `max_top_blast_radius` knobs can be raised, but the GitHub 65 535-byte cap is the hard ceiling. Larger PRs always benefit from the `change-summary.md` endpoint, which always returns full markdown.

### "Cache hit ratio is low"

- Expected the first time a `(head, base, job)` triple is queried. Subsequent renders should hit.
- A persistent miss rate means re-evaluations are producing different `head_sha` values (e.g., force-pushes). This is normal for active PRs.
- If miss rate is high without new commits, check for `RebuildImpactCache` write failures in the WARN log.

### "Metadata unavailable counter is non-zero"

- Means the eval pipeline did not populate `pname`/`version`/`license`/`maintainers` for any drv on the head side. Check the `nix-eval-jobs` invocation produced `meta` blocks.
- Affects display only — `Added`/`Removed` classification falls back to `RebuildOnly` rows.

---

## Alerts

Suggested Prometheus alert rules:

```yaml
- alert: ChangeSummaryEndToEndSlow
  expr: |
    histogram_quantile(0.95,
      sum by (le) (rate(eka_ci_change_summary_total_duration_seconds_bucket{phase="end_to_end"}[5m]))
    ) > 10
  for: 15m
  annotations:
    summary: "change-summary p95 > 10s"

- alert: ChangeSummaryCacheMissesElevated
  expr: |
    rate(eka_ci_change_summary_cache_misses_total[15m])
      / (rate(eka_ci_change_summary_cache_hits_total[15m])
         + rate(eka_ci_change_summary_cache_misses_total[15m])) > 0.5
  for: 30m
  annotations:
    summary: "change-summary cache miss ratio > 50%"

- alert: ChangeSummaryTruncationSpike
  expr: |
    sum(rate(eka_ci_change_summary_truncated_total{level="summary"}[15m])) > 0
  for: 30m
  annotations:
    summary: "change-summary collapsing tables to counts-only"
```
