-- Evaluation requests for pull-request head commits, keyed by commit SHA.
--
-- Webhooks and message passing are not reliable on their own: a webhook
-- can arrive while the server is down, and tasks waiting in in-memory
-- channels are lost on restart. The PR reconciler compares the open PRs
-- on GitHub against this table and GitHubJobSets, and re-requests the
-- evaluation of any head commit that was never evaluated.
--
--   requested_at  last time an evaluation was queued (epoch seconds)
--   attempts      evaluations queued by the reconciler (webhooks don't count)
--   completed_at  set when evaluation finished without producing a jobset
--                 (no jobs configured, or the eval failed); NULL otherwise
CREATE TABLE IF NOT EXISTS GitHubPrEvalRequests (
    owner        TEXT    NOT NULL,
    repo_name    TEXT    NOT NULL,
    head_sha     TEXT    NOT NULL,
    pr_number    INTEGER NOT NULL,
    requested_at INTEGER NOT NULL DEFAULT (unixepoch()),
    attempts     INTEGER NOT NULL DEFAULT 0,
    completed_at INTEGER,
    PRIMARY KEY (owner, repo_name, head_sha)
);

-- Count how many times a journaled task has been replayed. A task that
-- never completes (it hangs, or fails on every start) is dropped after a
-- bounded number of replays instead of blocking its service forever.
ALTER TABLE TaskJournal ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
