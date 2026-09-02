-- Migration: Task journal for durable message delivery
-- Date: 2026-09-01
-- Description:
--   Write-ahead log for in-flight service tasks. Each row represents a
--   message that was received by a service but has not yet been fully
--   processed. On clean completion the row is deleted; on crash the
--   surviving rows are replayed on startup, giving at-least-once
--   delivery semantics.

CREATE TABLE IF NOT EXISTS TaskJournal (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Logical service name, e.g. "git", "eval", "ingress".
    service     TEXT    NOT NULL,
    -- JSON-serialized task payload.
    payload     TEXT    NOT NULL,
    -- Timestamp when the task was journaled (epoch seconds).
    created_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Recovery queries filter by service name.
CREATE INDEX IF NOT EXISTS idx_task_journal_service
    ON TaskJournal (service);
