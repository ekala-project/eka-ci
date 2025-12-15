-- Table to track GitHub users who are approved to create jobsets
-- This helps prevent malicious users from exhausting resources with expensive builds
CREATE TABLE IF NOT EXISTS ApprovedUsers (
    ROWID INTEGER PRIMARY KEY,
    -- GitHub username (e.g., "octocat")
    github_username TEXT NOT NULL UNIQUE,
    -- GitHub user ID (numeric ID, more reliable than username which can change)
    github_id INTEGER NOT NULL UNIQUE,
    -- When this user was approved
    approved_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Optional notes about why this user was approved
    notes TEXT
);

-- Index for quick lookups by username
CREATE INDEX IF NOT EXISTS ApprovedUsersUsername ON ApprovedUsers (github_username);

-- Index for quick lookups by user ID
CREATE INDEX IF NOT EXISTS ApprovedUsersId ON ApprovedUsers (github_id);
