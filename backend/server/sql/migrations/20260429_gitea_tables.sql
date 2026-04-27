-- Migration: Add Gitea platform tables
-- Date: 2026-04-29
-- Description: Creates table set for Gitea support, mirroring the GitHub/GitLab tables structure.
--              Gitea's API is GitHub-compatible, so the schema is very similar to GitHub,
--              but includes domain support for self-hosted instances.

-- ============================================================================
-- Gitea Jobsets
-- ============================================================================
-- Tracks evaluation results (jobsets) for Gitea PRs and commits.
-- Analogous to GitHubJobSets with added domain column.
CREATE TABLE IF NOT EXISTS GiteaJobSets (
    ROWID INTEGER PRIMARY KEY,
    -- Git commit SHA
    sha TEXT NOT NULL,
    -- Job name (e.g., "ci", "nixpkgs-eval")
    job TEXT NOT NULL,
    -- Repository owner (user or organization)
    owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- Domain (e.g., "gitea.com", "gitea.example.com")
    domain TEXT NOT NULL DEFAULT 'gitea.com',
    -- Serialized CI config JSON for post-build hooks
    config_json TEXT,
    UNIQUE (sha, job, domain) ON CONFLICT IGNORE
);

CREATE INDEX IF NOT EXISTS GiteaJobSetsSha ON GiteaJobSets (sha);
CREATE INDEX IF NOT EXISTS GiteaJobSetsJobName ON GiteaJobSets (job);

-- ============================================================================
-- Gitea Jobs
-- ============================================================================
-- Join table linking jobsets to derivations, with difference tracking.
-- Analogous to the Job table for GitHub.
CREATE TABLE IF NOT EXISTS GiteaJob (
    -- Foreign key to GiteaJobSets.ROWID
    jobset INTEGER NOT NULL,
    -- Foreign key to Drv.ROWID
    drv_id INTEGER NOT NULL,
    -- Attribute path (e.g., "hello.x86_64-linux")
    name TEXT NOT NULL,
    -- Difference status: 0=New, 1=Changed, 2=Removed
    difference INTEGER NOT NULL,
    FOREIGN KEY (jobset) REFERENCES GiteaJobSets (ROWID) ON DELETE CASCADE,
    FOREIGN KEY (drv_id) REFERENCES Drv (ROWID) ON DELETE CASCADE,
    UNIQUE (jobset, name, drv_id)
);

CREATE INDEX IF NOT EXISTS GiteaJobJobsetId ON GiteaJob (jobset);
CREATE INDEX IF NOT EXISTS GiteaJobDrvId ON GiteaJob (drv_id);

-- ============================================================================
-- Gitea Pull Requests
-- ============================================================================
-- Tracks Gitea PR metadata (analogous to GitHubPullRequests).
-- Gitea uses GitHub-compatible PR numbering and API.
CREATE TABLE IF NOT EXISTS GiteaPullRequests (
    -- Gitea PR number (unique within a repository)
    pr_number INTEGER NOT NULL,
    -- Repository owner
    owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- Domain
    domain TEXT NOT NULL DEFAULT 'gitea.com',
    -- Head commit SHA (PR branch)
    head_sha TEXT NOT NULL,
    -- Base commit SHA (target branch)
    base_sha TEXT NOT NULL,
    -- PR title
    title TEXT NOT NULL,
    -- PR author's Gitea username
    author TEXT NOT NULL,
    -- PR state: "open", "closed"
    state TEXT NOT NULL,
    -- ISO 8601 timestamp when PR was created
    created_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last update
    updated_at TEXT NOT NULL,
    -- Auto-merge enabled flag
    auto_merge_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    -- Preferred merge method: "merge", "squash", "rebase", or NULL for repo default
    merge_method TEXT CHECK (merge_method IN ('merge', 'squash', 'rebase')),
    -- Comment-triggered merge request fields (analogous to GitHub comment_merge_*)
    comment_merge_sha TEXT,
    comment_merge_method TEXT CHECK (comment_merge_method IN ('merge', 'squash', 'rebase')),
    comment_merge_requester_id INTEGER,
    comment_merge_requester_login TEXT,
    comment_merge_comment_id INTEGER,
    comment_merge_requested_at TIMESTAMP,
    PRIMARY KEY (domain, owner, repo_name, pr_number)
);

-- Index for querying open PRs across all repos
CREATE INDEX IF NOT EXISTS GiteaPRsByState ON GiteaPullRequests (state, updated_at DESC);

-- Index for querying PRs by repository
CREATE INDEX IF NOT EXISTS GiteaPRsByRepo ON GiteaPullRequests (domain, owner, repo_name, state);

-- Index for looking up PRs by head commit SHA (to link with GiteaJobSets)
CREATE INDEX IF NOT EXISTS GiteaPRsByHeadSha ON GiteaPullRequests (head_sha);

-- Index for finding PRs with active comment-merge requests
CREATE INDEX IF NOT EXISTS GiteaPRsCommentMergePending
ON GiteaPullRequests(comment_merge_sha)
WHERE comment_merge_sha IS NOT NULL;

-- ============================================================================
-- Gitea Instances
-- ============================================================================
-- Tracks configured Gitea instances and repositories (analogous to GitHubInstallations).
-- Gitea can be self-hosted, so we track instances by domain and store credentials.
CREATE TABLE IF NOT EXISTS GiteaInstances (
    -- Domain (e.g., "gitea.com", "gitea.example.com")
    domain TEXT PRIMARY KEY,
    -- Instance display name
    name TEXT NOT NULL,
    -- ISO 8601 timestamp when instance was added to eka-ci
    added_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last metadata update
    updated_at TEXT NOT NULL
);

-- Track individual repositories within a Gitea instance
CREATE TABLE IF NOT EXISTS GiteaRepositories (
    -- Foreign key to GiteaInstances.domain
    domain TEXT NOT NULL,
    -- Gitea internal repository ID
    repo_id INTEGER NOT NULL,
    -- Repository owner
    owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- Repository visibility: "public", "private"
    is_private BOOLEAN NOT NULL,
    -- ISO 8601 timestamp when repository was added
    added_at TEXT NOT NULL,
    PRIMARY KEY (domain, repo_id),
    FOREIGN KEY (domain) REFERENCES GiteaInstances (domain) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS GiteaReposByOwner ON GiteaRepositories (domain, owner, repo_name);

-- ============================================================================
-- Gitea Check Runs
-- ============================================================================
-- Tracks Gitea check runs (analogous to GitHubCheckRuns).
-- Newer Gitea versions support GitHub-compatible Checks API.
-- Older versions fall back to commit statuses (stored here with synthetic IDs).
CREATE TABLE IF NOT EXISTS GiteaCheckRuns (
    -- Gitea check run ID (or synthetic ID for commit statuses)
    check_run_id INTEGER PRIMARY KEY,
    -- Git commit SHA
    sha TEXT NOT NULL,
    -- Check run name/context
    name TEXT NOT NULL,
    -- Domain
    domain TEXT NOT NULL DEFAULT 'gitea.com',
    -- Repository owner
    repo_owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- Foreign key to Drv.ROWID (if check is for a specific build)
    drv_id INTEGER,
    -- Check run state: "pending", "running", "success", "failure", "cancelled"
    state TEXT NOT NULL,
    -- True if this is a GitHub-style check run, false if it's a commit status
    is_check_run BOOLEAN NOT NULL DEFAULT TRUE,
    -- ISO 8601 timestamp when check was created
    created_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last update
    updated_at TEXT NOT NULL,
    FOREIGN KEY (drv_id) REFERENCES Drv (ROWID) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS GiteaCheckRunsSha ON GiteaCheckRuns (sha);
CREATE INDEX IF NOT EXISTS GiteaCheckRunsRepo ON GiteaCheckRuns (domain, repo_owner, repo_name);
CREATE INDEX IF NOT EXISTS GiteaCheckRunsDrvId ON GiteaCheckRuns (drv_id);
