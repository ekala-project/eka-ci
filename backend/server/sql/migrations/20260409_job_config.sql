-- Store job configuration for post-build hooks
-- This allows the RecorderService to access job config when builds complete
ALTER TABLE GitHubJobSets ADD COLUMN config_json TEXT;

-- Create table to track hook executions
CREATE TABLE IF NOT EXISTS HookExecution (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    drv_path TEXT NOT NULL,
    hook_name TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    exit_code INTEGER,
    success BOOLEAN,
    log_path TEXT NOT NULL,
    FOREIGN KEY (drv_path) REFERENCES Drv(drv_path) ON DELETE CASCADE
);

-- Index for querying hook executions by drv
CREATE INDEX IF NOT EXISTS HookExecutionDrv ON HookExecution (drv_path);

-- Index for querying hook executions by time
CREATE INDEX IF NOT EXISTS HookExecutionTime ON HookExecution (started_at);
