-- Each PR could create a "jobset". These are unique to that commit.
-- However, we want to determine what drvs have been added or removed
-- so we need a way to query (commit, job) on both the base and head commit
CREATE TABLE IF NOT EXISTS GitHubJobSets (
    ROWID INTEGER PRIMARY KEY,
    -- this is the git commit, github uses sha as the name
    sha TEXT NOT NULL,
    -- "job name". Used to determine the drv difference between two shas
    job TEXT NOT NULL,
    -- repository owner, e.g. github.com/{owner}/{repo}
    owner TEXT NOT NULL,
    -- repository name, e.g. github.com/{owner}/{repo}
    repo_name TEXT NOT NULL,
    UNIQUE (sha, job) ON CONFLICT IGNORE
);

-- We will be querying these frequently to determine dependency state
CREATE INDEX IF NOT EXISTS JobSha ON GitHubJobSets (sha);
CREATE INDEX IF NOT EXISTS JobName ON GitHubJobSets (job);

-- This is just a join table between drvs and github jobsets, and used
-- to determine which drvs are referenced by a jobset
CREATE TABLE IF NOT EXISTS Job (
    jobset INTEGER NOT NULL, -- identifier for a specific job on a commit
    drv_id INTEGER NOT NULL, -- drv which gets referenced by the jobset
    name TEXT NOT NULL, -- "job name" this will usually be the attrpath
    difference INTEGER NOT NULL DEFAULT 0, -- 0 = New, 1 = Changed, 2 = Removed
    UNIQUE (jobset, name, drv_id) ON CONFLICT IGNORE,
    FOREIGN KEY (jobset) REFERENCES GitHubJobSets(ROWID) ON DELETE CASCADE,
    FOREIGN KEY (drv_id) REFERENCES Drv(ROWID) ON DELETE CASCADE
);

-- Index for querying jobs by difference type
CREATE INDEX IF NOT EXISTS JobDifference ON Job (jobset, difference);

-- These are GitHub CI check_run's where the status is associated with the
-- build status of a drv
CREATE TABLE IF NOT EXISTS CheckRunInfo (
    check_run_id INTEGER PRIMARY KEY, -- identifier for a specific job on a commit
    drv_id INTEGER NOT NULL, -- drv which gets referenced by the jobset
    repo_name TEXT NOT NULL, -- repository name, e.g. github.com/{owner}/{repo}
    repo_owner TEXT NOT NULL, -- repository owner, e.g. github.com/{owner}/{repo}
    FOREIGN KEY (drv_id) REFERENCES Drv(ROWID) ON DELETE CASCADE
);

-- See if a view is easier than just a query
CREATE VIEW IF NOT EXISTS CheckRun AS
SELECT
  c.check_run_id,
  c.repo_name,
  c.repo_owner,
  d.build_state,
  d.drv_path
FROM CheckRunInfo AS c
JOIN Drv AS d ON d.ROWID = c.drv_id;
