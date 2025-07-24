-- This is the minimal amount of information needed to identify a build
-- This purposely tries to avoid details such as attr path which may
-- differ (e.g. python3.pkgs.setuptools vs python3Packages.setuptools)
CREATE TABLE IF NOT EXISTS Drv (
    drv_path TEXT NOT NULL PRIMARY KEY ON CONFLICT IGNORE,
    system TEXT NOT NULL,
    -- Allows for allocation of a build on a host which needs certain features
    -- For example, NixOS tests require "kvm nixos-test"
    required_system_features TEXT NULL,
    build_state INTEGER NOT NULL
);

-- These are the direct drv dependencies
-- Invert the relation to find direct "referrers"/"downstream drvs"
-- It should be that downstream dependencies can span many branches
-- For more documentation, see the corresponding Rust struct.
CREATE TABLE IF NOT EXISTS DrvRefs (
    referrer TEXT NOT NULL, -- downstream drv or consumer
    reference TEXT NOT NULL, -- upstream drv or dependency
    -- A primary key on this table is useless, as all accesses go through the explicit indexes
    -- anyways. To avoid duplicates entries, a unique constraint is put on the fields. By
    -- ignoring conflicting entries, the service can just not care about this constraint when
    -- inserting new entries.
    UNIQUE (referrer, reference) ON CONFLICT IGNORE,
    FOREIGN KEY (referrer) REFERENCES Drv(drv_path) ON DELETE CASCADE,
    FOREIGN KEY (reference) REFERENCES Drv(drv_path) ON DELETE RESTRICT
);

-- We will be querying these frequently to determine dependency state
CREATE INDEX IF NOT EXISTS DrvReferrer ON DrvRefs (referrer);
CREATE INDEX IF NOT EXISTS DrvReferrer ON DrvRefs (reference);
