-- Add table to track derivation output runtime references
-- This enables per-output tracking of sizes, closures, and retained dependencies
-- with proper foreign key relationship to Drv table

CREATE TABLE IF NOT EXISTS DrvRuntimeRefs (
    ROWID INTEGER PRIMARY KEY,
    drv_id INTEGER NOT NULL,          -- Foreign key to Drv(ROWID)
    output_name TEXT NOT NULL,        -- Output name: "out", "dev", "doc", "lib", etc.
    output_path TEXT NOT NULL,        -- Full store path: /nix/store/hash-name
    runtime_reference TEXT NOT NULL,  -- Full store path of runtime dependency
    output_size INTEGER,              -- Size of this specific output in bytes
    closure_size INTEGER,             -- Closure size of this specific output in bytes
    recorded_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(drv_id, output_name, runtime_reference),
    FOREIGN KEY (drv_id) REFERENCES Drv(ROWID) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_runtime_refs_drv ON DrvRuntimeRefs(drv_id);
CREATE INDEX IF NOT EXISTS idx_runtime_refs_output ON DrvRuntimeRefs(drv_id, output_name);
CREATE INDEX IF NOT EXISTS idx_runtime_refs_path ON DrvRuntimeRefs(output_path);
