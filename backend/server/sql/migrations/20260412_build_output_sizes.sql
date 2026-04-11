-- Migration: Build Output Size Tracking
-- Created: 2026-04-12
-- Purpose: Track output sizes for builds to detect installation bloat

-- Add output_size column to existing Drv table
ALTER TABLE Drv ADD COLUMN output_size INTEGER;

-- Create table for historical output size tracking
-- This allows comparing current build sizes against baseline (e.g., main branch)
CREATE TABLE IF NOT EXISTS DrvOutputSizes (
    ROWID INTEGER PRIMARY KEY,
    drv_path TEXT NOT NULL,              -- Derivation path
    output_size INTEGER NOT NULL,         -- Total output size in bytes
    git_commit TEXT NOT NULL,             -- Commit SHA where this size was measured
    git_repo TEXT NOT NULL,               -- Repository URL
    recorded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    -- Enable efficient baseline lookups: "get latest size for attr X on main branch"
    UNIQUE(drv_path, git_commit, git_repo)
);

-- Index for fast baseline queries: find sizes for a drv_path in a specific repo
CREATE INDEX IF NOT EXISTS DrvOutputSizesPath ON DrvOutputSizes (drv_path, git_repo);

-- Index for finding most recent measurements
CREATE INDEX IF NOT EXISTS DrvOutputSizesRecorded ON DrvOutputSizes (recorded_at DESC);

-- Index for querying by commit (useful for historical analysis)
CREATE INDEX IF NOT EXISTS DrvOutputSizesCommit ON DrvOutputSizes (git_commit);
