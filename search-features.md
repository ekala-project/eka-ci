# Search Index Generation for ekapkgs-cli

## Overview

ekapkgs-cli supports downloading a pre-built SQLite search database from a remote URL (via `defaults.index_url` in `config.toml`). This database powers:

- **Tab completion** for `ekapkgs home packages add <TAB>`, `ekapkgs home services add <TAB>`, etc.
- **Package search** via `ekapkgs search packages <query>`
- **File search** via `ekapkgs search files <query>` (replaces `nix-locate` dependency)

Without a pre-built database, the client falls back to running `nix search nixpkgs --json ^` locally, which takes 30-60 seconds. The pre-built SQLite database makes tab completion and search work instantly on first use with efficient FTS5 full-text search.

## What ekaci Produces

A single SQLite database (`search.db.zst`) per channel, regenerated whenever the tracked nixpkgs input is updated (e.g., on channel promotion or flake input pin change). The database contains FTS5 virtual tables for instant full-text search without loading the entire dataset into memory.

### Schema

```sql
-- Package metadata from nix search
CREATE TABLE packages (
    attr         TEXT PRIMARY KEY,  -- e.g. "hello", "python3Packages.requests"
    pname        TEXT NOT NULL,
    version      TEXT NOT NULL,
    description  TEXT,
    outputs      TEXT,              -- JSON array, e.g. '["out","dev"]'
    main_program TEXT               -- from meta.mainProgram
);

-- FTS5 index for package search
CREATE VIRTUAL TABLE packages_fts USING fts5(
    attr, pname, description,
    content='packages', content_rowid='rowid'
);

-- All files installed by successfully-built packages
CREATE TABLE files (
    file    TEXT NOT NULL,   -- e.g. "bin/hello", "lib/libz.so.1", "share/man/man1/hello.1.gz"
    package TEXT NOT NULL,   -- attribute path
    output  TEXT NOT NULL    -- usually "out"
);
CREATE INDEX idx_files_file    ON files(file);
CREATE INDEX idx_files_package ON files(package);

-- FTS5 index for file search
CREATE VIRTUAL TABLE files_fts USING fts5(
    file, package,
    content='files', content_rowid='rowid'
);

-- NixOS/ekaOS configuration options
CREATE TABLE options (
    name         TEXT PRIMARY KEY,
    description  TEXT,
    type         TEXT,
    default_val  TEXT,   -- JSON-serialized
    example      TEXT,   -- JSON-serialized
    declarations TEXT,   -- JSON array
    read_only    INTEGER NOT NULL DEFAULT 0
);

-- Generation metadata
CREATE TABLE metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys: generated_at, channel_name, nixpkgs_rev, package_count, file_count, option_count
```

### Data Sources

**Packages** (~120K entries):

```bash
nix search nixpkgs --json ^
```

Post-processing strips the `legacyPackages.{system}.` prefix from attribute paths. The `outputs` and `main_program` fields are enriched metadata not available from `nix search` — see [Enriched Metadata](#enriched-metadata) below.

**Time:** ~30-60 seconds for a full nixpkgs eval.

**Options** (ekaos-specific):

Evaluated from the ekaos configuration option tree. Only relevant for ekaos system flakes. The nix expression:

```bash
nix eval --impure --expr '
  let
    flake = builtins.getFlake "<flake-ref>";
    pkgs = flake.legacyPackages.${builtins.currentSystem};
    lib = pkgs.lib;
    eval = flake.config or
           (if builtins.hasAttr "ekaosConfigurations" flake then
             (builtins.head (builtins.attrValues flake.ekaosConfigurations))
           else { options = {}; });
    optionsList = lib.optionAttrSetToDocList (eval.options or {});
    filtered = builtins.filter (o:
      !(o.internal or false) && !(o.visible or true == false)
    ) optionsList;
  in builtins.toJSON (map (o: {
    name = o.name;
    description = o.description or "";
    type = o.type or "unspecified";
    default = builtins.tryEval (builtins.toJSON (o.default or null));
    example = builtins.tryEval (builtins.toJSON (o.example or null));
    declarations = o.declarations or [];
    readOnly = o.readOnly or false;
  }) filtered)
'
```

**Files** (tens of millions of entries):

For each successfully-built package in the channel, recursively walk the entire store path tree and collect all installed files and symlinks. This includes binaries, libraries, headers, man pages, configuration files, etc.

---

## Enriched Metadata

The basic package data (`pname`, `version`, `description`) comes from `nix search`. The enriched fields (`outputs`, `main_program`) require additional evaluation that ekaci is well-positioned to provide.

### `outputs` (list of output names)

After a successful build, the output names are available from `nix derivation show`:

```bash
nix derivation show /nix/store/...-hello-2.12.1.drv | jq '.[].outputs | keys'
# ["out"]
```

Or from `nix-eval-jobs` output, which ekaci already parses.

### `main_program` (primary binary name)

```bash
nix eval nixpkgs#hello.meta.mainProgram 2>/dev/null
# "hello"
```

### Integration with existing ekaci data

ekaci already tracks:
- Derivation paths and build outcomes in the `Drv` table
- Output sizes via `DrvOutputSize` and `DrvClosureSize`
- Dependency graphs via `DrvRefs`

The enriched metadata can be collected as a **post-build hook** or as part of the **recorder** phase when a build completes successfully.

---

## Hosting

The generated `search.db.zst` file needs to be served at a stable HTTP URL. The ekapkgs-cli client fetches `{index_url}/{channel_name}/search.db.zst`.

### Options

1. **S3 bucket** (recommended): ekaci already has S3 cache infrastructure. Upload the database alongside cache artifacts. Serve via CloudFront or direct S3 URL.

2. **ekapkgs-serve static endpoint**: Add `GET /indexes/{channel_name}/search.db.zst` to ekapkgs-serve's HTTP API. Serve files from a configured directory.

3. **Git branch**: Push the database to a dedicated branch (similar to channel promotion). Serve via raw Git hosting.

### Client configuration

Users add to `~/.config/ekapkgs/config.toml`:

```toml
[defaults]
index_url = "https://indexes.example.com"
```

The client then fetches:
- `https://indexes.example.com/{channel}/search.db.zst`

---

## Trigger & Frequency

### When to regenerate

- **Packages + files**: When the tracked nixpkgs flake input updates. This aligns with channel promotion — after a channel evaluates and promotes, regenerate the database for that channel's nixpkgs pin.

- **Options**: When the ekaos system flake changes (module/option definitions).

### Implementation approach

The search index is generated as a **post-channel-promotion task**: after `ChannelService` successfully promotes a commit, it sends a `GenerateIndexes` task to the `SearchIndexService`, which runs generators, builds the SQLite database, and uploads it.

---

## Why SQLite over compressed JSON

The previous design used zstd-compressed JSON arrays (`packages.json.zst`, `files.json.zst`, etc.). SQLite with FTS5 was chosen instead because:

1. **No full load required** — queries only touch relevant B-tree pages via indexes, rather than deserializing the entire dataset into memory
2. **FTS5 full-text search** — built-in, optimized for substring/prefix package and file queries
3. **Memory-mapped I/O** — the OS page cache handles hot data without explicit deserialization
4. **Single file** — one download per channel instead of multiple files
5. **Query flexibility** — SQL allows filtering, joining, and aggregation without client-side code
6. **Scalability** — the files index (tens of millions of entries covering all installed files) would be impractical to load fully into memory; SQLite handles it with constant memory via indexed queries
