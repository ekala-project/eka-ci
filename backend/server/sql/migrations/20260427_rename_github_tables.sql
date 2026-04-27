-- Migration: Rename GitHub-specific tables for multi-platform support
-- Date: 2026-04-27
-- Description: Renames PullRequests → GitHubPullRequests and CheckRunInfo → GitHubCheckRuns
--              to make room for platform-specific tables (GitLabMergeRequests, GiteaCheckRuns, etc.).
--              This is part of the multi-platform refactor where each platform gets its own set of tables.

-- Note: GitHubJobSets and GitHubInstallations already have platform-specific names, so they stay as-is.

-- Rename PullRequests → GitHubPullRequests
ALTER TABLE PullRequests RENAME TO GitHubPullRequests;

-- Rename CheckRunInfo → GitHubCheckRuns
ALTER TABLE CheckRunInfo RENAME TO GitHubCheckRuns;

-- Note: Indexes are automatically renamed by SQLite when using ALTER TABLE RENAME.
-- The following indexes now apply to GitHubPullRequests:
--   - PRsByState → (now indexes GitHubPullRequests)
--   - PRsByRepo → (now indexes GitHubPullRequests)
--   - PRsByHeadSha → (now indexes GitHubPullRequests)
--   - idx_pull_requests_comment_merge_pending → (now indexes GitHubPullRequests)

-- The following index now applies to GitHubCheckRuns:
--   - idx_checkruninfo_repo_owner → (now indexes GitHubCheckRuns)
