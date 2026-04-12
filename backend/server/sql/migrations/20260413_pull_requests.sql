-- Track Pull Request metadata
-- This table stores PR information to enable the review portal
CREATE TABLE IF NOT EXISTS PullRequests (
    -- GitHub PR number (unique within a repository)
    pr_number INTEGER NOT NULL,
    -- Repository owner, e.g. github.com/{owner}/{repo}
    owner TEXT NOT NULL,
    -- Repository name, e.g. github.com/{owner}/{repo}
    repo_name TEXT NOT NULL,
    -- Head commit SHA (the PR branch)
    head_sha TEXT NOT NULL,
    -- Base commit SHA (target branch, usually main/master)
    base_sha TEXT NOT NULL,
    -- PR title
    title TEXT NOT NULL,
    -- PR author's GitHub login
    author TEXT NOT NULL,
    -- PR state: "open", "closed", or "merged"
    state TEXT NOT NULL,
    -- ISO 8601 timestamp when PR was created
    created_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last update
    updated_at TEXT NOT NULL,
    PRIMARY KEY (owner, repo_name, pr_number)
);

-- Index for querying open PRs across all repos
CREATE INDEX IF NOT EXISTS PRsByState ON PullRequests (state, updated_at DESC);

-- Index for querying PRs by repository
CREATE INDEX IF NOT EXISTS PRsByRepo ON PullRequests (owner, repo_name, state);

-- Index for looking up PRs by head commit SHA (to link with GitHubJobSets)
CREATE INDEX IF NOT EXISTS PRsByHeadSha ON PullRequests (head_sha);
