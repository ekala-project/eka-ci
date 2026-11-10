-- Migration: Size baselines keyed by attr path on a base branch
-- Created: 2026-11-09
-- Purpose: Let size checks compare a PR build against the same attr on the
-- configured base branch. The previous baseline query was keyed by drv path,
-- which changes whenever a PR changes the package, and joined
-- DrvBuildMetadata, which nothing writes to, so no baseline was ever found.

-- Branch whose tip this jobset was evaluated as: set for PR-base and
-- tracking-branch push evaluations, NULL for PR heads and older rows.
ALTER TABLE GitHubJobSets ADD COLUMN branch TEXT;

-- Latest known sizes for an attr on a branch. Written from the Drv row
-- whenever a derivation referenced by a base-branch jobset has a recorded
-- size: when the base jobset is created (the drv was usually built earlier,
-- as a PR head) and when such a derivation finishes building.
CREATE TABLE IF NOT EXISTS DrvSizeBaselines (
    ROWID INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,                  -- Repository owner
    repo_name TEXT NOT NULL,              -- Repository name
    branch TEXT NOT NULL,                 -- Base branch, e.g. "main"
    job TEXT NOT NULL,                    -- Jobset job name from .ekaci/config.json
    attr_path TEXT NOT NULL,              -- Job.name (the attr path)
    system TEXT NOT NULL,                 -- Drv.system
    drv_path TEXT NOT NULL,               -- Drv the sizes were measured from (DrvId form)
    git_commit TEXT NOT NULL,             -- Base-branch commit that referenced drv_path
    output_size INTEGER,                  -- Total output (NAR) size in bytes
    closure_size INTEGER,                 -- Total closure size in bytes
    recorded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (owner, repo_name, branch, job, attr_path, system)
);
