-- Table to track authenticated users with their GitHub OAuth tokens
-- Used for user authentication and authorization
CREATE TABLE IF NOT EXISTS AuthenticatedUsers (
    ROWID INTEGER PRIMARY KEY,
    -- GitHub user ID (numeric ID, immutable)
    github_id INTEGER NOT NULL UNIQUE,
    -- GitHub username (e.g., "octocat", can change)
    github_username TEXT NOT NULL,
    -- GitHub avatar URL for display
    github_avatar_url TEXT,
    -- Encrypted GitHub OAuth access token (for making GitHub API calls on behalf of user)
    github_access_token TEXT NOT NULL,
    -- Whether this user has admin privileges
    is_admin BOOLEAN NOT NULL DEFAULT 0,
    -- When this user first authenticated
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Last time this user logged in
    last_login TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Index for quick lookups by user ID
CREATE INDEX IF NOT EXISTS AuthenticatedUsersId ON AuthenticatedUsers (github_id);

-- Index for quick lookups by username
CREATE INDEX IF NOT EXISTS AuthenticatedUsersUsername ON AuthenticatedUsers (github_username);

-- Index for finding admin users
CREATE INDEX IF NOT EXISTS AuthenticatedUsersAdmin ON AuthenticatedUsers (is_admin);
