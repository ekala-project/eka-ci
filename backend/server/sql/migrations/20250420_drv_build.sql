-- For documentation, see the corresponding Rust struct.
CREATE TABLE IF NOT EXISTS DrvBuildMetadata (
    derivation TEXT NOT NULL,
    build_attempt INTEGER NOT NULL,
    git_repo TEXT NOT NULL,
    git_commit TEXT NOT NULL,
    build_command TEXT NOT NULL, -- JSON encoded
    PRIMARY KEY (derivation, build_attempt)
);

-- Speed up queries that want to retrieve the metadata for all build attempts of a specific
-- derivation (also useful for queries that use MAX on the build_attempt to get the newest
-- metadata only).
CREATE INDEX IF NOT EXISTS DrvBuildMetadataDerivation ON DrvBuildMetadata (derivation);

-- For documentation, see the corresponding Rust struct.
CREATE TABLE IF NOT EXISTS DrvBuildEvent (
    -- Overwrite the default ROWID to enforce a strict monotonically increasing value. This is
    -- necessary to ensure any MAX selects on this field do not brake if a row is deleted (the
    -- default ROWID algorithm is allowed to reuse no longer used values).
    rowid INTEGER PRIMARY KEY AUTOINCREMENT, -- not present in the Rust struct
    derivation TEXT NOT NULL,
    build_attempt INTEGER NOT NULL,
    state INTEGER NOT NULL,
    timestamp TEXT NOT NULL DEFAULT (unixepoch())
);

-- Speed up queries that want to retrieve the state for a specific derivation.
CREATE INDEX IF NOT EXISTS DrvBuildEventDerivation ON DrvBuildEvent (derivation);

-- Speed up queries that want to retrieve all derivations in a specific state.
CREATE INDEX IF NOT EXISTS DrvBuildEventState ON DrvBuildEvent (state);

-- For documentation, see the corresponding Rust struct.
CREATE TABLE IF NOT EXISTS DrvRefs (
    referrer TEXT NOT NULL,
    reference TEXT NOT NULL,
    -- A primary key on this table is useless, as all accesses go through the explicit indexes
    -- anyways. To avoid duplicates entries, a unique constraint is put on the fields. By
    -- ignoring conflicting entries, the service can just not care about this constraint when
    -- inserting new entries.
    UNIQUE (referrer, reference) ON CONFLICT IGNORE
);

-- Speed up queries that want to retrieve a derivation's dependencies.
CREATE INDEX IF NOT EXISTS DrvRefsReferrer ON DrvRefs (referrer);

-- Speed up queries that want to retrieve a derivation's dependants.
CREATE INDEX IF NOT EXISTS DrvRefsReference ON DrvRefs (reference);
