-- Migration: Build Closure Size Tracking
-- Created: 2026-04-15
-- Purpose: Track closure sizes for builds to monitor dependency bloat

-- Add closure_size column to existing Drv table
ALTER TABLE Drv ADD COLUMN closure_size INTEGER;

-- Create table for historical closure size tracking
-- This allows comparing current build closure sizes against baseline (e.g., main branch)
CREATE TABLE IF NOT EXISTS DrvClosureSizes (
    ROWID INTEGER PRIMARY KEY,
    drv_path TEXT NOT NULL,              -- Derivation path
    closure_size INTEGER NOT NULL,        -- Total closure size in bytes (all dependencies)
    git_commit TEXT NOT NULL,             -- Commit SHA where this size was measured
    git_repo TEXT NOT NULL,               -- Repository URL
    recorded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    -- Enable efficient baseline lookups: "get latest closure size for attr X on main branch"
    UNIQUE(drv_path, git_commit, git_repo)
);

-- Index for fast baseline queries: find sizes for a drv_path in a specific repo
CREATE INDEX IF NOT EXISTS DrvClosureSizesPath ON DrvClosureSizes (drv_path, git_repo);

-- Index for finding most recent measurements
CREATE INDEX IF NOT EXISTS DrvClosureSizesRecorded ON DrvClosureSizes (recorded_at DESC);

-- Index for querying by commit (useful for historical analysis)
CREATE INDEX IF NOT EXISTS DrvClosureSizesCommit ON DrvClosureSizes (git_commit);
