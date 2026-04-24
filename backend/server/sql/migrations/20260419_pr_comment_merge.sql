-- Migration: Add @eka-ci merge comment command support for Pull Requests
-- Date: 2026-04-19
-- Description: Adds fields to track a pending comment-driven merge request on a PR.
--              Distinct from auto_merge_enabled (which is UI/maintainer-opt-in) to keep
--              the two paths semantically separate: a comment-driven merge captures the
--              exact head SHA at comment time and is cancelled on SHA drift, whereas
--              auto-merge rolls forward with the PR.
--
--              There is at most one active comment-merge request per PR; a new
--              `@eka-ci merge` comment on the same PR overwrites the previous row.

-- SHA captured at the time the `@eka-ci merge` command was issued.
-- When all gates go green, we verify the current head_sha == comment_merge_sha
-- before merging. If it has drifted, we cancel the request and react/comment.
ALTER TABLE PullRequests
ADD COLUMN comment_merge_sha TEXT;

-- Optional merge method arg from the comment (merge, squash, rebase).
-- NULL means "use repo default / PR preferred method".
ALTER TABLE PullRequests
ADD COLUMN comment_merge_method TEXT
    CHECK (comment_merge_method IN ('merge', 'squash', 'rebase'));

-- GitHub user id of the requester (for later permission re-check / audit).
ALTER TABLE PullRequests
ADD COLUMN comment_merge_requester_id INTEGER;

-- GitHub login of the requester (denormalized for logging / UI display).
ALTER TABLE PullRequests
ADD COLUMN comment_merge_requester_login TEXT;

-- Comment ID that triggered the merge request (for posting reactions).
ALTER TABLE PullRequests
ADD COLUMN comment_merge_comment_id INTEGER;

-- Timestamp when the merge was requested.
ALTER TABLE PullRequests
ADD COLUMN comment_merge_requested_at TIMESTAMP;

-- Index for finding PRs with an active comment-merge request, scoped to
-- only the pending rows to keep the index tight.
CREATE INDEX idx_pull_requests_comment_merge_pending
ON PullRequests(comment_merge_sha)
WHERE comment_merge_sha IS NOT NULL;
