-- Store GraphQL node IDs for check runs and repositories.
-- These are required for batched GraphQL mutations which use a
-- separate rate limit budget from the REST API.

ALTER TABLE GitHubCheckRuns ADD COLUMN node_id TEXT;
ALTER TABLE GitHubInstallationRepositories ADD COLUMN node_id TEXT;

-- Recreate the CheckRun view to include node_id.
-- The original view (20251108_github.sql) joins GitHubCheckRuns → Drv.
-- The later view (20260212_checks.sql) joins CheckRunInfo → CheckResult.
-- We need to handle both: the GitHub check runs view should include node_id.
DROP VIEW IF EXISTS CheckRun;
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
