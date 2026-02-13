-- Checks are similar to jobs but execute sandboxed imperative commands
-- instead of evaluating Nix expressions. Each PR can have multiple checks
-- that run commands in a sandboxed checkout of the repository.

-- CheckSets track which checks were run for a specific commit
CREATE TABLE IF NOT EXISTS GitHubCheckSets (
    ROWID INTEGER PRIMARY KEY,
    -- this is the git commit, github uses sha as the name
    sha TEXT NOT NULL,
    -- "check name". Used to identify the check
    check_name TEXT NOT NULL,
    -- repository owner, e.g. github.com/{owner}/{repo}
    owner TEXT NOT NULL,
    -- repository name, e.g. github.com/{owner}/{repo}
    repo_name TEXT NOT NULL,
    UNIQUE (sha, check_name) ON CONFLICT IGNORE
);

-- Index for querying checks by sha
CREATE INDEX IF NOT EXISTS CheckSetSha ON GitHubCheckSets (sha);
CREATE INDEX IF NOT EXISTS CheckSetName ON GitHubCheckSets (check_name);

-- CheckResults store the output of each check execution
CREATE TABLE IF NOT EXISTS CheckResult (
    ROWID INTEGER PRIMARY KEY,
    checkset INTEGER NOT NULL, -- identifier for a specific check on a commit
    success BOOLEAN NOT NULL, -- whether the check passed
    exit_code INTEGER NOT NULL, -- exit code from the command
    stdout TEXT, -- standard output (may be truncated)
    stderr TEXT, -- standard error (may be truncated)
    duration_ms INTEGER NOT NULL, -- execution time in milliseconds
    executed_at INTEGER NOT NULL, -- unix timestamp when check was executed
    FOREIGN KEY (checkset) REFERENCES GitHubCheckSets(ROWID) ON DELETE CASCADE
);

-- Index for querying results by checkset
CREATE INDEX IF NOT EXISTS CheckResultCheckSet ON CheckResult (checkset);
CREATE INDEX IF NOT EXISTS CheckResultSuccess ON CheckResult (checkset, success);

-- CheckRunInfo links GitHub check_run IDs to check results
-- This is similar to how derivation builds are tracked for jobs
CREATE TABLE IF NOT EXISTS CheckRunInfo (
    check_run_id INTEGER PRIMARY KEY, -- GitHub check_run ID
    check_result_id INTEGER NOT NULL, -- reference to CheckResult
    repo_name TEXT NOT NULL,
    repo_owner TEXT NOT NULL,
    FOREIGN KEY (check_result_id) REFERENCES CheckResult(ROWID) ON DELETE CASCADE
);

-- View for easily querying check run status
CREATE VIEW IF NOT EXISTS CheckRun AS
SELECT
    c.check_run_id,
    c.repo_name,
    c.repo_owner,
    r.success,
    r.exit_code,
    r.duration_ms,
    r.executed_at
FROM CheckRunInfo AS c
JOIN CheckResult AS r ON r.ROWID = c.check_result_id;
