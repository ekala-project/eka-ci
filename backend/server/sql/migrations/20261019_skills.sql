-- AI agent skill evaluation results.
-- Each row is linked to a GitHubCheckSets row via checkset_id,
-- the same way CheckResult rows are.
CREATE TABLE IF NOT EXISTS SkillResult (
    id        INTEGER PRIMARY KEY,
    checkset_id INTEGER NOT NULL,
    success   INTEGER NOT NULL,          -- 1=pass, 0=fail
    error     INTEGER NOT NULL DEFAULT 0, -- 1=model error (not a pass/fail decision)
    reason    TEXT    NOT NULL,
    findings  TEXT    NOT NULL,           -- JSON array of findings
    model_id  TEXT    NOT NULL,           -- Which model was used
    duration_ms INTEGER NOT NULL,
    FOREIGN KEY (checkset_id) REFERENCES GitHubCheckSets(ROWID)
);
