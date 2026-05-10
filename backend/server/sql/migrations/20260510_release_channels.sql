-- Migration: Release channels (gated branch promotion)
-- Date: 2026-05-10
-- Description:
--   Records the lifecycle of a release-channel promotion attempt: a push
--   to a tracking-branch (e.g. "master") triggers an evaluation of the
--   channel's `required` and `packages` jobsets at that SHA, and on
--   success the validated SHA is fast-forwarded onto the channel's
--   target-branch (e.g. "ekapkgs-unstable").
--
--   Channel *configuration* (which repos have which channels) lives in
--   server admin config (ekaci.toml) — only promotion *state* is stored
--   in the database, so the table key is a channel_id string of the form
--   "<forge>/<owner>/<repo>/<channel-name>" rather than a foreign key.
--
--   No behaviour is wired to this table by the migration alone; the
--   schema is added first so subsequent PRs can land incrementally.

-- ============================================================================
-- ChannelPromotion
-- ============================================================================
-- One row per (channel, tracking_sha) attempt.
-- An attempt is "in flight" while status = 0 (Evaluating); a UNIQUE partial
-- index ensures at most one in-flight row per channel, which the
-- ChannelService relies on to enforce its coalescing semantics ("don't
-- drop the current evaluation, but skip intermediate commits").
CREATE TABLE IF NOT EXISTS ChannelPromotion (
    rowid INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Stable channel identifier: "<forge>/<owner>/<repo>/<name>".
    -- forge is one of: "github", "gitlab", "gitea:<domain>".
    channel_id TEXT NOT NULL,
    -- The SHA on the tracking-branch that this attempt evaluates.
    tracking_sha TEXT NOT NULL,
    -- Snapshot of the configured target-branch at attempt time, recorded
    -- so historical rows remain meaningful even if the admin later
    -- renames the target-branch in ekaci.toml.
    target_branch TEXT NOT NULL,
    -- The SHA target-branch pointed at immediately before this attempt
    -- (NULL if the branch did not yet exist remotely on first promotion).
    previous_target_sha TEXT,
    -- 0=Evaluating  1=Blocked  2=Promoted  3=PushFailed  4=Skipped
    -- Skipped is recorded for SHAs that the coalescer observed but
    -- deliberately bypassed because a newer SHA had already arrived.
    status INTEGER NOT NULL,
    -- JSON object describing why a Blocked or PushFailed row failed
    -- (e.g. {"failed_required":["pkg-a","pkg-b"]} or {"non_ff": true}).
    -- NULL for Evaluating/Promoted/Skipped rows.
    blocked_reason TEXT,
    -- JSON map of {required_job_name: "Success"|"Failure"|"TransitiveFailure"|...}
    -- captured at decision time so future debugging never requires joining
    -- against the live build history with timestamp guesswork.
    required_results TEXT,
    -- Unix epoch seconds.
    started_at INTEGER NOT NULL,
    -- NULL while Evaluating; set when transitioning to a terminal status.
    completed_at INTEGER
);

-- Lookup history for a channel newest-first.
CREATE INDEX IF NOT EXISTS ChannelPromotionByChannel
    ON ChannelPromotion (channel_id, started_at DESC);

-- At most one in-flight (Evaluating) attempt per channel.
CREATE UNIQUE INDEX IF NOT EXISTS ChannelPromotionInFlight
    ON ChannelPromotion (channel_id)
    WHERE status = 0;

-- Idempotency lookup: has this (channel, sha) already reached a terminal
-- decision?  Used by the ChannelService idempotency guard to refuse
-- re-promoting a SHA whose decision is already recorded, which prevents
-- replayed webhooks from re-driving the pipeline.
CREATE INDEX IF NOT EXISTS ChannelPromotionByChannelSha
    ON ChannelPromotion (channel_id, tracking_sha);
