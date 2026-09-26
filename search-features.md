# Search Index Generation for ekapkgs-cli

## Overview

ekapkgs-cli now supports downloading pre-built search indexes from a remote URL (via `defaults.index_url` in `config.toml`). These indexes power:

- **Tab completion** for `ekapkgs home packages add <TAB>`, `ekapkgs home services add <TAB>`, etc.
- **Package search** via `ekapkgs search packages <query>`
- **File search** via `ekapkgs search files <query>` (replaces `nix-locate` dependency)

Without pre-built indexes, the client falls back to running `nix search nixpkgs --json ^` locally, which takes 30-60 seconds. Pre-built indexes make tab completion and search work instantly on first use.

## What ekaci Needs to Produce

Four zstd-compressed JSON index files, regenerated whenever the tracked nixpkgs input is updated (e.g., on channel promotion or flake input pin change).

### 1. `packages.json.zst` (~3MB compressed)

**Generation:**

```bash
nix search nixpkgs --json ^ > /tmp/raw.json
```

**Post-processing:** The raw output is a map `{ "legacyPackages.x86_64-linux.attr": { pname, version, description } }`. Transform to a flat array and strip the `legacyPackages.{system}.` prefix:

```json
[
  {
    "attr": "hello",
    "pname": "hello",
    "version": "2.12.1",
    "description": "A program that produces a familiar, friendly greeting",
    "outputs": ["out"],
    "main_program": "hello"
  }
]
```

The `outputs` and `main_program` fields are enriched metadata not available from `nix search`. See [Enriched Metadata](#enriched-metadata) below.

**Time:** ~30-60 seconds for a full nixpkgs eval.

### 2. `options.json.zst` (~500KB compressed)

**Generation:** Evaluate the ekaos configuration's option tree. The ekapkgs-cli already has the exact nix expression for this (see `crates/ekapkgs/src/commands/search.rs`, `generate_option_index()`). The core logic:

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

**Scope:** This is only relevant for ekaos system flakes. If ekaci tracks a non-ekaos repo, this index can be skipped.

### 3. `service-options.json.zst` (~100KB compressed)

**Generation:** Evaluate the service module schema from the ekaos flake. The ekapkgs-cli has the generator at `crates/ekapkgs/src/service_schema.rs`. The nix expression imports `generate-service-options.nix` from the flake's service infrastructure.

**Scope:** Only for repos with ekaos service modules. Skip otherwise.

### 4. `files.json.zst` (~30MB compressed, executables only)

**Generation:** For each successfully built package in the channel, walk the store path's file tree and collect executables under `bin/`, `sbin/`, and `libexec/`.

```json
[
  { "file": "bin/hello", "package": "hello", "output": "out" },
  { "file": "bin/python3", "package": "python3", "output": "out" },
  { "file": "bin/python3-config", "package": "python3", "output": "out" }
]
```

**How to get file listings:** After a successful build, ekaci already has the store path. Use:

```bash
nix path-info --json /nix/store/...-hello-2.12.1
# Then walk the store path for executables:
find /nix/store/...-hello-2.12.1/{bin,sbin,libexec} -type f -o -type l 2>/dev/null
```

Or use `nix nar ls --json /nix/store/...-hello-2.12.1` for a structured listing without needing the path to be locally present (works on cached narinfo).

**Note:** Only index executables to keep the file index manageable (~30MB vs ~300MB for all files). The client's `ekapkgs search files` command searches this index by substring match.

---

## Enriched Metadata

The basic package index (`pname`, `version`, `description`) can be generated from `nix search` alone. The enriched fields (`outputs`, `main_program`) require additional evaluation that ekaci is well-positioned to provide.

### `outputs` (list of output names)

After a successful build, the output names are available from `nix derivation show`:

```bash
nix derivation show /nix/store/...-hello-2.12.1.drv | jq '.[].outputs | keys'
# ["out"]
```

Or from `nix-eval-jobs` output, which ekaci already parses — the `NixEvalDrv` struct may include output info.

### `main_program` (primary binary name)

```bash
nix eval nixpkgs#hello.meta.mainProgram 2>/dev/null
# "hello"
```

This can be bulk-evaluated with a single nix expression across all packages:

```nix
let pkgs = import <nixpkgs> {};
in builtins.mapAttrs (name: drv:
  builtins.tryEval (drv.meta.mainProgram or null)
) pkgs
```

### Integration with existing ekaci data

ekaci already tracks:
- Derivation paths and build outcomes in the `Drv` table
- Output sizes via `DrvOutputSize` and `DrvClosureSize`
- Dependency graphs via `DrvRefs`

The enriched metadata can be collected as a **post-build hook** or as part of the **recorder** phase when a build completes successfully. The recorder already calls `nix path-info` for size data — extending it to also capture output names is minimal.

---

## Hosting

The generated `.json.zst` files need to be served at a stable HTTP URL. The ekapkgs-cli client fetches `{index_url}/{name}.json.zst`.

### Options

1. **S3 bucket** (recommended): ekaci already has S3 cache infrastructure. Upload index files alongside cache artifacts. Serve via CloudFront or direct S3 URL.

2. **ekapkgs-serve static endpoint**: Add `GET /indexes/{name}.json.zst` to ekapkgs-serve's HTTP API. Serve files from a configured directory.

3. **Git branch**: Push index files to a dedicated branch (similar to channel promotion). Serve via raw Git hosting.

### Client configuration

Users add to `~/.config/ekapkgs/config.toml`:

```toml
[defaults]
index_url = "https://indexes.example.com"
```

The client then fetches:
- `https://indexes.example.com/packages.json.zst`
- `https://indexes.example.com/options.json.zst`
- `https://indexes.example.com/service-options.json.zst`
- `https://indexes.example.com/files.json.zst`

---

## Trigger & Frequency

### When to regenerate

- **`packages.json.zst`**: When the tracked nixpkgs flake input updates. This aligns with channel promotion — after a channel evaluates and promotes, regenerate the package index for that channel's nixpkgs pin.

- **`options.json.zst`** and **`service-options.json.zst`**: When the ekaos system flake changes (module/option definitions).

- **`files.json.zst`**: After a channel promotion completes and all required packages have been built. This is the most expensive index but also the least latency-sensitive — it can run as a background job.

### Implementation approach

The most natural fit within ekaci's architecture is as a **post-channel-promotion hook**: after `ChannelService` successfully promotes a commit, trigger index generation as a follow-up task.

Alternatively, it could be a periodic job (e.g., hourly cron) that checks whether the nixpkgs pin has changed since the last index generation.

---

## Manifest (optional)

A metadata file at `{index_url}/manifest.json` would allow the client to check staleness without downloading full indexes:

```json
{
  "generated_at": "2026-09-23T12:00:00Z",
  "nixpkgs_rev": "abc123def456...",
  "indexes": {
    "packages": { "size": 3145728, "entries": 120000 },
    "options": { "size": 524288, "entries": 8500 },
    "service-options": { "size": 102400, "entries": 340 },
    "files": { "size": 31457280, "entries": 2100000 }
  }
}
```

This is not required for the initial implementation but would enable future optimizations like conditional downloads and client-side staleness warnings.
