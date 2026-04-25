-- Read-through cache for rebuild-impact computations.
--
-- The aggregated rebuild-impact analysis (per-system rebuild counts +
-- multi-source BFS for top blast-radius drvs) is a deterministic function
-- of the (head_sha, base_sha, jobset) triple plus the underlying Job/Drv
-- graph at evaluation time. Once both jobsets exist and have been
-- evaluated, the answer never changes — repeated PR check posts and UI
-- queries can be served from cache.
--
-- See docs/design-package-change-rebuild-impact.md §5.2 + §10.
--
-- Invalidation:
--   * head SHA changes naturally produce a new row (different PK).
--   * Stale entries are pruned by a nightly cleanup that deletes rows
--     where computed_at < datetime('now', '-7 days').
--
-- summary_json holds the full serialised RebuildImpactResponse (envelope
-- + per_system + total_unique_drvs) so the cache hit can return the wire
-- response verbatim. The denormalised columns (rebuild_count,
-- critical_path_drv, blast_radius) carry the headline numbers for cheap
-- aggregate lookups (e.g. dashboard list views) without parsing JSON.
CREATE TABLE RebuildImpactCache (
    head_sha          TEXT NOT NULL,
    base_sha          TEXT NOT NULL,
    jobset            TEXT NOT NULL,
    rebuild_count     INTEGER NOT NULL,
    critical_path_drv TEXT,
    blast_radius      INTEGER,
    summary_json      TEXT NOT NULL,
    computed_at       TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (head_sha, base_sha, jobset)
);

-- Lookups by head_sha are common (dashboard / PR detail views surface
-- "what's the rebuild impact of this commit across all base comparisons").
CREATE INDEX idx_rebuild_impact_head ON RebuildImpactCache(head_sha);
