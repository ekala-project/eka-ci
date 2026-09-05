-- Coalesced variant gates: allow multiple drv_ids per check_run_id.
--
-- Previously GitHubCheckRuns had check_run_id as sole PRIMARY KEY, meaning
-- each GitHub check run could only map to a single derivation. With coalesced
-- gates, a single check run (e.g., "linux") represents multiple variants
-- (linux.v6_17, linux.v6_18, etc.), so we need a composite PK.

-- Drop the view first so SQLite doesn't complain about dangling references.
DROP VIEW IF EXISTS CheckRun;

-- SQLite doesn't support ALTER TABLE DROP CONSTRAINT, so recreate the table.
CREATE TABLE GitHubCheckRunsNew (
    check_run_id INTEGER NOT NULL,
    drv_id INTEGER NOT NULL,
    repo_name TEXT NOT NULL,
    repo_owner TEXT NOT NULL,
    node_id TEXT,
    PRIMARY KEY (check_run_id, drv_id),
    FOREIGN KEY (drv_id) REFERENCES Drv(ROWID) ON DELETE CASCADE
);

INSERT INTO GitHubCheckRunsNew (check_run_id, drv_id, repo_name, repo_owner, node_id)
    SELECT check_run_id, drv_id, repo_name, repo_owner, node_id
    FROM GitHubCheckRuns;

DROP TABLE GitHubCheckRuns;
ALTER TABLE GitHubCheckRunsNew RENAME TO GitHubCheckRuns;

-- Recreate indexes
CREATE INDEX idx_githubcheckruns_drv_id ON GitHubCheckRuns(drv_id);
CREATE INDEX idx_githubcheckruns_repo_owner ON GitHubCheckRuns(repo_owner, repo_name);

-- Recreate the CheckRun view (from 20260904_checkrun_node_ids.sql)
CREATE VIEW CheckRun AS
SELECT
  c.check_run_id,
  c.repo_name,
  c.repo_owner,
  d.build_state,
  d.drv_path,
  c.node_id
FROM GitHubCheckRuns AS c
JOIN Drv AS d ON d.ROWID = c.drv_id;
