-- Migration: Add auto-merge support for Pull Requests
-- Date: 2026-04-16
-- Description: Adds fields to track auto-merge enablement, merge method preferences,
--              and merge completion details for pull requests.

-- Add auto_merge_enabled column
-- Whether auto-merge is enabled for this PR (maintainer opt-in)
ALTER TABLE PullRequests
ADD COLUMN auto_merge_enabled BOOLEAN NOT NULL DEFAULT FALSE;

-- Add merge_method column
-- Preferred merge method (merge, squash, rebase). NULL means use repo default.
ALTER TABLE PullRequests
ADD COLUMN merge_method TEXT CHECK (merge_method IN ('merge', 'squash', 'rebase'));

-- Add merged_by_user_id column
-- User ID who triggered the merge (can be automated system)
ALTER TABLE PullRequests
ADD COLUMN merged_by_user_id INTEGER;

-- Add merged_at column
-- Timestamp when the PR was merged via the auto-merge system
ALTER TABLE PullRequests
ADD COLUMN merged_at TIMESTAMP;

-- Index for finding PRs with auto-merge enabled
CREATE INDEX idx_pull_requests_auto_merge
ON PullRequests(auto_merge_enabled)
WHERE auto_merge_enabled = TRUE;

-- Index for tracking merged PRs
CREATE INDEX idx_pull_requests_merged_at
ON PullRequests(merged_at)
WHERE merged_at IS NOT NULL;
