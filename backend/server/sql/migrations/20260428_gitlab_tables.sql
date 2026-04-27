-- Migration: Add GitLab platform tables
-- Date: 2026-04-28
-- Description: Creates table set for GitLab support, mirroring the GitHub tables structure.
--              Each platform (GitHub, GitLab, Gitea) gets its own set of tables to avoid
--              cross-platform coupling and enable platform-specific optimizations.

-- ============================================================================
-- GitLab Jobsets
-- ============================================================================
-- Tracks evaluation results (jobsets) for GitLab MRs and commits.
-- Analogous to GitHubJobSets but with GitLab-specific fields.
CREATE TABLE IF NOT EXISTS GitLabJobSets (
    ROWID INTEGER PRIMARY KEY,
    -- Git commit SHA
    sha TEXT NOT NULL,
    -- Job name (e.g., "ci", "nixpkgs-eval")
    job TEXT NOT NULL,
    -- Repository owner/group (e.g., "gitlab-org")
    owner TEXT NOT NULL,
    -- Repository name (e.g., "gitlab")
    repo_name TEXT NOT NULL,
    -- GitLab numeric project ID (used in API calls)
    project_id INTEGER NOT NULL,
    -- Domain (e.g., "gitlab.com", "gitlab.example.com")
    domain TEXT NOT NULL DEFAULT 'gitlab.com',
    -- Serialized CI config JSON for post-build hooks
    config_json TEXT,
    UNIQUE (sha, job, domain) ON CONFLICT IGNORE
);

CREATE INDEX IF NOT EXISTS GitLabJobSetsSha ON GitLabJobSets (sha);
CREATE INDEX IF NOT EXISTS GitLabJobSetsJobName ON GitLabJobSets (job);
CREATE INDEX IF NOT EXISTS GitLabJobSetsProjectId ON GitLabJobSets (project_id);

-- ============================================================================
-- GitLab Jobs
-- ============================================================================
-- Join table linking jobsets to derivations, with difference tracking.
-- Analogous to the Job table for GitHub.
CREATE TABLE IF NOT EXISTS GitLabJob (
    -- Foreign key to GitLabJobSets.ROWID
    jobset INTEGER NOT NULL,
    -- Foreign key to Drv.ROWID
    drv_id INTEGER NOT NULL,
    -- Attribute path (e.g., "hello.x86_64-linux")
    name TEXT NOT NULL,
    -- Difference status: 0=New, 1=Changed, 2=Removed
    difference INTEGER NOT NULL,
    FOREIGN KEY (jobset) REFERENCES GitLabJobSets (ROWID) ON DELETE CASCADE,
    FOREIGN KEY (drv_id) REFERENCES Drv (ROWID) ON DELETE CASCADE,
    UNIQUE (jobset, name, drv_id)
);

CREATE INDEX IF NOT EXISTS GitLabJobJobsetId ON GitLabJob (jobset);
CREATE INDEX IF NOT EXISTS GitLabJobDrvId ON GitLabJob (drv_id);

-- ============================================================================
-- GitLab Merge Requests
-- ============================================================================
-- Tracks GitLab MR metadata (analogous to GitHubPullRequests).
CREATE TABLE IF NOT EXISTS GitLabMergeRequests (
    -- GitLab MR IID (unique within a project, not globally)
    mr_iid INTEGER NOT NULL,
    -- Repository owner/group
    owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- GitLab numeric project ID
    project_id INTEGER NOT NULL,
    -- Domain
    domain TEXT NOT NULL DEFAULT 'gitlab.com',
    -- Head commit SHA (source branch)
    head_sha TEXT NOT NULL,
    -- Base commit SHA (target branch)
    base_sha TEXT NOT NULL,
    -- MR title
    title TEXT NOT NULL,
    -- MR author's GitLab username
    author TEXT NOT NULL,
    -- MR state: "opened", "closed", "merged", "locked"
    state TEXT NOT NULL,
    -- ISO 8601 timestamp when MR was created
    created_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last update
    updated_at TEXT NOT NULL,
    -- Auto-merge enabled flag
    auto_merge_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    -- Preferred merge method: "merge", "squash", "rebase_merge", or NULL for project default
    merge_method TEXT CHECK (merge_method IN ('merge', 'squash', 'rebase_merge')),
    -- Comment-triggered merge request fields (analogous to GitHub comment_merge_*)
    comment_merge_sha TEXT,
    comment_merge_method TEXT CHECK (comment_merge_method IN ('merge', 'squash', 'rebase_merge')),
    comment_merge_requester_id INTEGER,
    comment_merge_requester_username TEXT,
    comment_merge_note_id INTEGER,  -- GitLab calls comments "notes"
    comment_merge_requested_at TIMESTAMP,
    PRIMARY KEY (domain, project_id, mr_iid)
);

-- Index for querying open MRs across all repos
CREATE INDEX IF NOT EXISTS GitLabMRsByState ON GitLabMergeRequests (state, updated_at DESC);

-- Index for querying MRs by project
CREATE INDEX IF NOT EXISTS GitLabMRsByProject ON GitLabMergeRequests (domain, project_id, state);

-- Index for looking up MRs by head commit SHA (to link with GitLabJobSets)
CREATE INDEX IF NOT EXISTS GitLabMRsByHeadSha ON GitLabMergeRequests (head_sha);

-- Index for finding MRs with active comment-merge requests
CREATE INDEX IF NOT EXISTS GitLabMRsCommentMergePending
ON GitLabMergeRequests(comment_merge_sha)
WHERE comment_merge_sha IS NOT NULL;

-- ============================================================================
-- GitLab Projects
-- ============================================================================
-- Tracks configured GitLab projects (analogous to GitHubInstallations).
-- Stores project metadata and access credentials.
CREATE TABLE IF NOT EXISTS GitLabProjects (
    -- GitLab numeric project ID (globally unique within a GitLab instance)
    project_id INTEGER NOT NULL,
    -- Domain (e.g., "gitlab.com", "gitlab.example.com")
    domain TEXT NOT NULL DEFAULT 'gitlab.com',
    -- Project path with namespace (e.g., "gitlab-org/gitlab")
    path_with_namespace TEXT NOT NULL,
    -- Project owner/group
    owner TEXT NOT NULL,
    -- Project name
    name TEXT NOT NULL,
    -- Project visibility: "public", "internal", "private"
    visibility TEXT NOT NULL,
    -- ISO 8601 timestamp when project was added to eka-ci
    added_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last metadata update
    updated_at TEXT NOT NULL,
    PRIMARY KEY (domain, project_id)
);

CREATE INDEX IF NOT EXISTS GitLabProjectsByPath ON GitLabProjects (domain, path_with_namespace);

-- ============================================================================
-- GitLab Commit Statuses
-- ============================================================================
-- Tracks GitLab commit statuses (analogous to GitHubCheckRuns).
-- GitLab doesn't have a rich Checks API like GitHub, so we use commit statuses
-- and post detailed results as MR comments.
CREATE TABLE IF NOT EXISTS GitLabCommitStatuses (
    -- GitLab commit status ID (returned by GitLab API)
    status_id INTEGER PRIMARY KEY,
    -- Git commit SHA
    sha TEXT NOT NULL,
    -- Status name/context (e.g., "eka-ci/build")
    name TEXT NOT NULL,
    -- GitLab numeric project ID
    project_id INTEGER NOT NULL,
    -- Domain
    domain TEXT NOT NULL DEFAULT 'gitlab.com',
    -- Repository owner/group
    repo_owner TEXT NOT NULL,
    -- Repository name
    repo_name TEXT NOT NULL,
    -- Foreign key to Drv.ROWID (if status is for a specific build)
    drv_id INTEGER,
    -- Status state: "pending", "running", "success", "failed", "canceled"
    state TEXT NOT NULL,
    -- ISO 8601 timestamp when status was created
    created_at TEXT NOT NULL,
    -- ISO 8601 timestamp of last update
    updated_at TEXT NOT NULL,
    FOREIGN KEY (drv_id) REFERENCES Drv (ROWID) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS GitLabCommitStatusesSha ON GitLabCommitStatuses (sha);
CREATE INDEX IF NOT EXISTS GitLabCommitStatusesProject ON GitLabCommitStatuses (domain, project_id);
CREATE INDEX IF NOT EXISTS GitLabCommitStatusesDrvId ON GitLabCommitStatuses (drv_id);
