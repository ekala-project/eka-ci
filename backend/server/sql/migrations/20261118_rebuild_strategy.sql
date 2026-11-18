-- Migration: Selective rebuilds ("direct-referrers" rebuild strategy)
-- Created: 2026-11-17
-- Purpose: Let a job build only the packages a PR changes and their direct
-- referrers instead of every rebuilt package.

-- Whether a Drv row was traversed into the dependency graph (its closure and
-- DrvRefs were recorded). Evaluation also writes a row for every job drv so
-- Job rows can reference it; those eval-only rows (traversed = 0) are never
-- loaded into the graph, so jobs skipped by a rebuild strategy and base-branch
-- job drvs are not scheduled after a restart. Existing rows keep the previous
-- behaviour (traversed = 1).
ALTER TABLE Drv ADD COLUMN traversed INTEGER NOT NULL DEFAULT 1;

-- Job rows that the jobset's rebuild strategy decided not to build. Skipped
-- jobs keep their New/Changed difference (so change summaries still show the
-- full set of rebuilt packages) but are excluded from dispatch, check runs and
-- jobset completion.
ALTER TABLE Job ADD COLUMN skipped INTEGER NOT NULL DEFAULT 0;

-- SHA-256 over the job's sorted `output=path` pairs. Comparing it against the
-- base jobset tells whether a job's outputs actually change, which (unlike the
-- drv path) is stable across fixed-output derivation churn.
ALTER TABLE Job ADD COLUMN outputs_hash TEXT;
