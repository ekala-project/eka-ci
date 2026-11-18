-- Migration: Store each pull request's web URL
-- Created: 2026-11-18
-- Purpose: The web UI links to a PR on its forge. Building that link from
-- owner/repo would hard-code github.com and break GitHub Enterprise; the
-- forge's own `html_url` is stored instead. NULL for merge-queue rows and for
-- PRs not updated since this migration (the UI falls back to its own PR page).
ALTER TABLE GitHubPullRequests ADD COLUMN html_url TEXT;
