-- This is the minimal amount of information needed to identify a build
-- This purposely tries to avoid details such as attr path which may
-- differ (e.g. python3.pkgs.setuptools vs python3Packages.setuptools)
CREATE TABLE IF NOT EXISTS Drv (
    ROWID INTEGER PRIMARY KEY NOT NULL,
    drv_path TEXT NOT NULL,
    system TEXT NOT NULL,
    -- Allows for allocation of a build on a host which needs certain features
    -- For example, NixOS tests require "kvm nixos-test"
    required_system_features TEXT NULL,
    -- Whether this is a Fixed-Output Derivation (has a known output hash)
    -- FODs are fetches (fetchurl, fetchgit, etc.) that get dedicated build resources
    is_fod BOOLEAN NOT NULL DEFAULT 0,
    build_state INTEGER NOT NULL,
    UNIQUE (drv_path) ON CONFLICT IGNORE
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

-- Index for filtering FOD builds in queries
CREATE INDEX IF NOT EXISTS DrvIsFod ON Drv (is_fod);

-- Tracks which drvs are blocked by failed dependencies
-- When a drv fails, all transitive referrers are marked as TransitiveFailure
-- and entries are added to this table showing the dependency chain
CREATE TABLE IF NOT EXISTS TransitiveFailure (
    failed_drv TEXT NOT NULL,     -- The drv that failed and is blocking others
    drv_referrer TEXT NOT NULL,   -- A drv that transitively depends on failed_drv
    UNIQUE (failed_drv, drv_referrer) ON CONFLICT IGNORE,
    FOREIGN KEY (failed_drv) REFERENCES Drv(drv_path) ON DELETE CASCADE,
    FOREIGN KEY (drv_referrer) REFERENCES Drv(drv_path) ON DELETE CASCADE
);

-- Index for quick lookups of what failed dependencies affect a drv
CREATE INDEX IF NOT EXISTS TransitiveFailureByReferrer ON TransitiveFailure (drv_referrer);

-- Index for quick lookups of what drvs are affected by a failure
CREATE INDEX IF NOT EXISTS TransitiveFailureByFailed ON TransitiveFailure (failed_drv);
