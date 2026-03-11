-- Track GitHub App installations
-- This table stores information about where the GitHub App is installed
CREATE TABLE IF NOT EXISTS GitHubInstallations (
    installation_id INTEGER PRIMARY KEY,
    account_type TEXT NOT NULL, -- "Organization" or "User"
    account_login TEXT NOT NULL, -- GitHub org or user login name
    installed_at TEXT NOT NULL, -- ISO 8601 timestamp when app was installed
    updated_at TEXT NOT NULL, -- ISO 8601 timestamp of last update
    suspended_at TEXT NULL -- ISO 8601 timestamp when suspended, NULL if active
);

-- Track which repositories are accessible in each installation
-- This allows us to show all repos where the app is installed, even if no builds have run yet
CREATE TABLE IF NOT EXISTS GitHubInstallationRepositories (
    installation_id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL, -- GitHub's internal repository ID
    repo_name TEXT NOT NULL, -- Repository name (e.g., "my-repo")
    repo_owner TEXT NOT NULL, -- Repository owner (e.g., "octocat")
    added_at TEXT NOT NULL, -- ISO 8601 timestamp when repo was added to installation
    PRIMARY KEY (installation_id, repo_id),
    FOREIGN KEY (installation_id) REFERENCES GitHubInstallations(installation_id) ON DELETE CASCADE
);

-- Index for looking up repositories by owner and name
CREATE INDEX IF NOT EXISTS InstallationReposByOwner ON GitHubInstallationRepositories (repo_owner, repo_name);

-- Index for looking up all repos for a specific installation
CREATE INDEX IF NOT EXISTS InstallationReposByInstallation ON GitHubInstallationRepositories (installation_id);
