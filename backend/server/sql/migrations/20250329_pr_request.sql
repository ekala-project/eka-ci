-- These are effectively queued builds to be attempted
CREATE TABLE IF NOT EXISTS Drv (
    id INTEGER PRIMARY KEY,
    drv_path TEXT NOT NULL UNIQUE,
    was_successful BOOLEAN NULL DEFAULT NULL,
    build_attempt_count INTEGER NOT NULL DEFAULT 0,
    platform TEXT NOT NULL,
    requested_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- These are the direct drv dependencies
-- Invert the relation to find direct "referrers"/"downstream drvs"
-- It should be that downstream dependencies can span many branches
CREATE TABLE IF NOT EXISTS DrvRefs (
    referrer INTEGER NOT NULL, -- downstream drv or consumer
    reference INTEGER NOT NULL, -- upstream drv or dependency
    PRIMARY KEY (referrer, reference),
    FOREIGN KEY (referrer) REFERENCES Drv(id) ON DELETE CASCADE,
    FOREIGN KEY (reference) REFERENCES Drv(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS DrvIndexReferrer on DrvRefs(referrer);
CREATE INDEX IF NOT EXISTS DrvIndexReference on DrvRefs(reference);

CREATE TABLE IF NOT EXISTS DrvOutputs (
    drv  INTEGER NOT NULL,
    attr TEXT NOT NULL, -- symbolic output attr, usually "out"
    path TEXT NOT NULL, -- E.g. /nix/store/<hash>-<drv name>-<attr>
    PRIMARY KEY (drv, attr),
    FOREIGN KEY (drv) REFERENCES Drv(id) ON DELETE CASCADE
);
