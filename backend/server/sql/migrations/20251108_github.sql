-- Each PR could create a "jobset". These are unique to that commit.
-- However, we want to determine what drvs have been added or removed
-- so we need a way to query (commit, job) on both the base and head commit
CREATE TABLE IF NOT EXISTS GitHubJobSets (
    ROWID INTEGER PRIMARY KEY,
    -- this is the git commit, github uses sha as the name
    sha TEXT NOT NULL,
    -- "job name". Used to determine the drv difference between two shas
    job TEXT NOT NULL,
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
    UNIQUE (jobset, name, drv_id) ON CONFLICT IGNORE,
    FOREIGN KEY (jobset) REFERENCES GitHubJobSets(ROWID) ON DELETE CASCADE,
    FOREIGN KEY (drv_id) REFERENCES Drv(ROWID) ON DELETE CASCADE
);

