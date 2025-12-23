-- Table to track GitHub App installations
-- Used to persist installation data and track installation lifecycle
CREATE TABLE IF NOT EXISTS GitHubInstallations (
    ROWID INTEGER PRIMARY KEY,
    -- GitHub installation ID (numeric ID, immutable)
    installation_id INTEGER NOT NULL UNIQUE,
    -- GitHub account ID that installed the app
    account_id INTEGER NOT NULL,
    -- GitHub account login (username or organization name)
    account_login TEXT NOT NULL,
    -- Type of account: 'User' or 'Organization'
    account_type TEXT NOT NULL,
    -- Repository selection: 'all' or 'selected'
    repository_selection TEXT NOT NULL,
    -- Number of selected repositories (NULL if 'all')
    selected_repositories_count INTEGER,
    -- When this installation was suspended (NULL if active)
    suspended_at TIMESTAMP,
    -- When this installation was created on GitHub
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Last time this installation was updated
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Index for quick lookups by installation ID
CREATE INDEX IF NOT EXISTS GitHubInstallationsId ON GitHubInstallations (installation_id);

-- Index for quick lookups by account login
CREATE INDEX IF NOT EXISTS GitHubInstallationsAccountLogin ON GitHubInstallations (account_login);

-- Index for quick lookups by account ID
CREATE INDEX IF NOT EXISTS GitHubInstallationsAccountId ON GitHubInstallations (account_id);
