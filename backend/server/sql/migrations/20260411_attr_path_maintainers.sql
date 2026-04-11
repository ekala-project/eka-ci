-- Migration: AttrPathMaintainers table
-- Created: 2026-04-11
-- Purpose: Track which users are maintainers of specific attribute paths

CREATE TABLE IF NOT EXISTS AttrPathMaintainers (
    ROWID INTEGER PRIMARY KEY,
    attr_path TEXT NOT NULL,                     -- The attribute path (e.g., "nixpkgs.python3")
    github_user_id INTEGER NOT NULL,              -- GitHub user ID of the maintainer
    added_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    added_by_user_id INTEGER,                     -- GitHub user ID of admin who added this maintainer (NULL if self-added)

    -- Foreign key to AuthenticatedUsers table
    FOREIGN KEY (github_user_id) REFERENCES AuthenticatedUsers(github_id) ON DELETE CASCADE,
    FOREIGN KEY (added_by_user_id) REFERENCES AuthenticatedUsers(github_id) ON DELETE SET NULL,

    -- Prevent duplicate maintainer assignments
    UNIQUE(attr_path, github_user_id)
);

-- Index for fast lookup of maintainers by attr path
CREATE INDEX IF NOT EXISTS AttrPathMaintainersPath ON AttrPathMaintainers (attr_path);

-- Index for fast lookup of attr paths by maintainer
CREATE INDEX IF NOT EXISTS AttrPathMaintainersUserId ON AttrPathMaintainers (github_user_id);

-- Composite index for efficient privilege checking
CREATE INDEX IF NOT EXISTS AttrPathMaintainersPathUser ON AttrPathMaintainers (attr_path, github_user_id);
