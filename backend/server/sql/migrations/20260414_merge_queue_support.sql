-- Add merge queue support to PullRequests table
-- This allows storing merge queue builds alongside regular PR builds

-- Add is_merge_queue column to distinguish merge queue entries from regular PRs
ALTER TABLE PullRequests ADD COLUMN is_merge_queue BOOLEAN NOT NULL DEFAULT FALSE;

-- Add merge_group_head_sha to store the original merge group commit SHA
-- For regular PRs this will be NULL, for merge queue builds it stores the gh-readonly-queue/* commit
ALTER TABLE PullRequests ADD COLUMN merge_group_head_sha TEXT;

-- Index for querying merge queue builds
CREATE INDEX IF NOT EXISTS PRsMergeQueue ON PullRequests (owner, repo_name, is_merge_queue, updated_at DESC);

-- Index for looking up merge queue builds by their merge group SHA
CREATE INDEX IF NOT EXISTS PRsByMergeGroupSha ON PullRequests (merge_group_head_sha) WHERE merge_group_head_sha IS NOT NULL;
