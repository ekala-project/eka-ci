-- This is the minimal amount of information needed to identify a build
-- This purposely tries to avoid details such as attr path which may
-- differ (e.g. python3.pkgs.setuptools vs python3Packages.setuptools)
CREATE TABLE IF NOT EXISTS Drv (
    id INTEGER PRIMARY KEY,
    drv_path TEXT NOT NULL,
    system TEXT NOT NULL
);

-- We will likely be looking up by drv path quite a bit
CREATE INDEX IF NOT EXISTS IndexDrvPath ON Drv(drv_path);

-- These are the direct drv dependencies
-- Invert the relation to find direct "referrers"/"downstream drvs"
-- It should be that downstream dependencies can span many branches
-- For more documentation, see the corresponding Rust struct.
CREATE TABLE IF NOT EXISTS DrvRefs (
    referrer INTEGER NOT NULL, -- downstream drv or consumer
    reference INTEGER NOT NULL, -- upstream drv or dependency
    -- A primary key on this table is useless, as all accesses go through the explicit indexes
    -- anyways. To avoid duplicates entries, a unique constraint is put on the fields. By
    -- ignoring conflicting entries, the service can just not care about this constraint when
    -- inserting new entries.
    UNIQUE (referrer, reference) ON CONFLICT IGNORE,
    FOREIGN KEY (referrer) REFERENCES Drv(id) ON DELETE CASCADE,
    FOREIGN KEY (reference) REFERENCES Drv(id) ON DELETE RESTRICT
);
