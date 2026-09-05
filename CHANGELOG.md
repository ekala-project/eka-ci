# Changelog

## 0.1.0

Initial release. EkaCI is a Nix-native CI system designed to answer
"should I merge this PR?" as quickly as possible.

### Multi-forge CI pipeline

- Receive webhooks from GitHub, GitLab, and Gitea to automatically
  evaluate and build PRs on push, open, and synchronize events
- Verify webhook signatures (HMAC-SHA256) with per-forge rate limiting
- Configure per-repository CI jobs via `.ekaci/config.json` with Nix
  file paths, cache references, and size check thresholds
- Evaluate Nix expressions via `nix-eval-jobs` with configurable safety
  limits (max entries, max stdout bytes, max line size) to prevent OOM
  from adversarial or oversized flakes
- Build derivations locally or on remote builders read from
  `/etc/nix/machines`, with platform-partitioned queues
  (`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`)
  and separate fixed-output derivation pools
- Automatically retry failed builds once before marking as permanent
  failure
- Report per-derivation build status as forge check runs with log tails

### Dependency-aware scheduling

- Track derivation dependency graphs in-memory for O(1) buildability
  lookups
- Skip builds whose outputs are already in a substitution cache
- Propagate failures transitively: when a build fails, mark all
  downstream dependents as blocked
- Unblock dependents automatically when a previously-failed build
  succeeds on retry
- LRU eviction (configurable, default 100k nodes) to bound memory on
  large repositories

### Change summary and rebuild impact

- Classify package changes between base and head: added, removed,
  version bump, renamed, rebuild-only, license change, maintainer change
- Compute per-system rebuild counts and blast radius (transitive
  dependent count via BFS)
- Post pre-rendered Markdown summaries as check runs, with progressive
  truncation to fit GitHub's 65k-char limit
- Cache rebuild impact analysis with 7-day TTL
- Expose JSON endpoints for programmatic consumption

### Post-build cache pushing

- Push successful build outputs to configured caches (nix copy, Cachix,
  Attic)
- Scope cache access per-repository and per-branch with glob patterns
- Load credentials from 10 sources: env vars, files, AWS profiles,
  Cachix tokens, HashiCorp Vault, AWS Secrets Manager, systemd
  credentials, instance metadata, GitHub App keys

### Release channels

- Gate branch promotion on CI results: only fast-forward a target branch
  (e.g. `nixpkgs-unstable`) when all required jobs pass at a tracking
  SHA
- Coalesce rapid pushes so only the latest SHA is evaluated
- Support dry-run mode for testing channel configuration without pushing

### Auto-merge

- Enable UI-driven auto-merge requiring all builds to pass and
  per-package maintainer approval
- Merge via PR comment (`@eka-ci merge [squash|rebase|merge|cancel]`)
  with SHA-pinning: new commits after the command cancel the merge with
  a notification
- Validate requested merge method against the repository's allowed
  methods

### GitHub-specific integrations

- GitHub App support with multi-app credential routing per repository
- GitHub OAuth for web UI authentication
- Merge queue support (`merge_group` events) with optional approval
  gating
- Workflow approval flow for fork PRs
- GraphQL batched check run updates to reduce API calls

### Crash recovery

- Persist in-flight tasks in a SQLite write-ahead journal; replay
  pending work on restart
- Normalize incomplete build states on startup (e.g. `Building` back
  to `Queued`)
- Reconstitute garbage-collected `.drv` files by re-evaluating the
  original jobset

### Web UI

- Elm single-page application with pages for builds, PRs, repositories,
  commits, jobsets, individual derivations, and admin
- Real-time build updates via WebSocket
- GitHub OAuth login

### CLI client

- `ekaci pr` / `jobs` / `jobset` — inspect PR status, active jobs, and
  per-jobset derivation state
- `ekaci drv info` / `drv deps` — query derivation status and
  dependency trees
- `ekaci log` — stream build logs
- `ekaci build` / `job` / `repo` — trigger builds, evaluations, and
  repo config reads
- `ekaci check` — run repo checks locally in a sandboxed environment
- `ekaci channel status` — query release channel promotion history
- `ekaci resync-checks` — re-sync check run states to GitHub

### Observability

- Prometheus metrics endpoint (`/metrics`)
- Structured logging via `tracing`

### Server configuration

- TOML config (`~/.config/ekaci/ekaci.toml`) with CLI and environment
  variable overrides
- Configurable build timeouts (output-based and absolute wall-clock),
  graph capacity, approval requirements, CORS origins, and default merge
  method
