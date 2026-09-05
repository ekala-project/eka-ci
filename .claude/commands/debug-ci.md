---
description: Debug CI build failures, inspect build progress, diagnose root causes, and retry failed builds for the EkaCI Nix build system
---

# CI Debugging Workflow

You are debugging CI issues for the EkaCI Nix build system. The backend lives at `backend/` and the CLI is `ekaci`. All commands below assume you're in the `backend/` directory.

## Prerequisites

- Server must be running. Check with: `pgrep -f eka_ci_server || echo "not running"`
- To start: `source secrets/ekaci-creds.sh && RUST_LOG=info nohup cargo run --bin eka_ci_server > /tmp/ekaci-server.log 2>&1 &`
- CLI: `cargo run --bin ekaci --`

## Step 1: Get an Overview

```bash
# List all active jobsets with progress counts
cargo run --bin ekaci -- jobs

# Show a specific jobset's details
cargo run --bin ekaci -- jobset <ID>

# Show PR status (falls back to jobset listing if PR not in DB)
cargo run --bin ekaci -- pr <owner> <repo> <pr_number>
```

The `jobs` output shows per-jobset: OK (success), Fail (failures + transitive), Build (building), Queue (queued + buildable), Total.

## Step 2: Find Failures

```bash
# Show only failed/blocked derivations in a jobset
cargo run --bin ekaci -- jobset <ID> --failures

# Check a specific drv's status via unix socket
cargo run --bin ekaci -- drv info <drv_path>
```

Build states: `Queued`, `Buildable`, `FailedRetry`, `Building`, `Completed(Success)`, `Completed(Failure)`, `TransitiveFailure`, `Interrupted(*)`, `Blocked`, `UnsatisfiableRequirements`.

## Step 3: Diagnose Root Causes

**For TransitiveFailure**: The package itself is fine — a dependency failed. Find the root:

```bash
# Show dependencies and their build states
cargo run --bin ekaci -- drv deps <drv_path>
```

Follow the chain: if a dep also shows `TransitiveFailure`, check *its* deps until you find the `Completed(Failure)` root.

**For Completed(Failure)**: Check the build log:

```bash
cargo run --bin ekaci -- log <drv_path>
```

Common log patterns:
- `"path '...' is required, but there is no substituter"` → drv was garbage collected. The reconstitution system should handle this on retry.
- Build errors → genuine build failure in the package.
- Timeout/OOM → resource limits hit.

## Step 4: Retry Failed Builds

```bash
# Force rebuild a single drv (resets to Queued, retries)
cargo run --bin ekaci -- build <drv_path> --force

# Force rebuild ALL failed drvs system-wide
cargo run --bin ekaci -- build dummy --force --rebuild-all
```

Note: `--rebuild-all` rebuilds failed drvs across ALL jobsets, not just one. Use sparingly.

## Step 5: Trigger CI for a PR

```bash
# Trigger full CI evaluation for a GitHub PR
cargo run --bin ekaci -- github github.com <owner> <repo> <pr_number>
```

This fetches the PR, checks out base+head commits, evaluates the nix file, creates GitHub check runs for each changed package, and queues builds.

## Common Patterns

### Mass TransitiveFailure after GC
One low-level dep was garbage collected, causing cascading failures. Find the root `Completed(Failure)` drv, check its log for the "no substituter" message, then `--force` rebuild it. The reconstitution system will re-evaluate the nix expression to recreate the `.drv` file before building.

### Queued drvs not progressing
Drvs stay `Queued` when their dependencies haven't finished building. Check `drv deps` to see what's blocking. If blockers are in a different jobset (base branch), they'll complete as that jobset's builds finish.

### Build queue saturation
When `jobs` shows many `Building` drvs and a large `Queue`, the builder is at capacity. PR drvs share the queue with base branch drvs. Drvs are processed FIFO — PR drvs don't have priority.

### Server not responding
Check the log: `tail -50 /tmp/ekaci-server.log`. Common issues: SQLite lock contention, graph channel overflow, or the ingress blocking on a full builder channel. Restart the server.

## API Quick Reference

The web API at `http://127.0.0.1:3030/v1/` can be queried directly for data the CLI doesn't expose:

| Endpoint | Description |
|----------|-------------|
| `GET /builds/active` | Active jobsets + building drvs |
| `GET /jobs/{id}` | Jobset state counts |
| `GET /jobs/{id}/drvs?state=X` | Drvs filtered by state |
| `GET /drvs/{drv}` | Single drv details |
| `GET /drvs/{drv}/dependencies` | Dependency list with states |
| `GET /logs/{drv}` | Build log (plain text) |
| `GET /prs` | Open PRs with build stats |
| `GET /repositories/{o}/{r}/jobsets` | Repo jobset history |
