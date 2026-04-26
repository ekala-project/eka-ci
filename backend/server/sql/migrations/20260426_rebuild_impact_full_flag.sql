-- Cache rows differ by `compute_full_blast_radius`: the seeds-only mode
-- (default) caps BFS at the top-50 cheapest seeds, while the full mode
-- runs BFS over every changed seed. The two answers can disagree, so the
-- cache key must include the flag.

ALTER TABLE RebuildImpactCache RENAME TO RebuildImpactCache_old;

CREATE TABLE RebuildImpactCache (
    head_sha           TEXT NOT NULL,
    base_sha           TEXT NOT NULL,
    jobset             TEXT NOT NULL,
    full_blast_radius  INTEGER NOT NULL DEFAULT 0,
    rebuild_count      INTEGER NOT NULL,
    critical_path_drv  TEXT,
    blast_radius       INTEGER,
    summary_json       TEXT NOT NULL,
    computed_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (head_sha, base_sha, jobset, full_blast_radius)
);

INSERT INTO RebuildImpactCache
    (head_sha, base_sha, jobset, full_blast_radius,
     rebuild_count, critical_path_drv, blast_radius,
     summary_json, computed_at)
SELECT
    head_sha, base_sha, jobset, 0,
    rebuild_count, critical_path_drv, blast_radius,
    summary_json, computed_at
FROM RebuildImpactCache_old;

DROP TABLE RebuildImpactCache_old;

CREATE INDEX idx_rebuild_impact_head ON RebuildImpactCache(head_sha);
