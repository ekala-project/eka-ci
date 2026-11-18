# Changelog

## Unreleased

### Configuration

- New `[[admins]]` setting (`services.eka-ci.settings.admins` in the NixOS
  module): GitHub users listed there are added to the PR-approval allow-list
  and promoted to admin on every startup, and granted admin on web-UI login.
  Previously both lived only in the database, so a fresh database left
  `require_approval` blocking every PR with no admin able to approve anyone

### Scheduler

- Buildability re-checks are no longer dropped when the ingress channel is
  full. The recorder pushed a `CheckBuildable` per referrer of every
  successful build with `try_send`, and while an eval filled the 50k
  channel with `EvalRequest`s these were dropped (37k in minutes on a large
  PR), leaving drvs `Queued` until the 60s sweep. Re-checks now go into a
  deduplicated, unbounded queue that ingress drains in batches alongside
  the channel, so they also no longer wait behind slow `EvalRequest`s
- The recorder no longer blocks on the ingress channel when re-queueing a
  retry, removing a possible recorder ↔ ingress deadlock
- On startup, Queued drvs whose dependencies have all succeeded are
  re-checked right away, not only Buildable ones
- The graph service no longer logs `UpdateState caller dropped the response
  oneshot` for every state change sent by the recorder and builder threads;
  those updates never wait for a reply, and the warning was spurious

### PR evaluation

- A PR head is no longer evaluated before its base: when no jobset for the
  base commit exists or has been sent, the head eval first evaluates the
  base itself. Previously a missing base (e.g. a head task replayed from
  the journal after a restart, carrying an outdated base) classified every
  job as `New` and built the whole job set
- A PR base whose eval has errors still records its jobset, so its heads
  are diffed against it instead of all being new
- A PR head jobset diffed against another base than the PR's current one
  (base branch moved, PR retargeted) is deleted and the head re-evaluated,
  instead of keeping the stale New/Changed classification
- Open PRs and live merge-queue groups are now reconciled every 5 minutes
  (and shortly after startup): any head commit with no jobset and no
  evaluation in flight is queued for evaluation, at most 3 times per head
  commit. Previously a PR pushed or a group queued while the server was
  down, or whose queued tasks were lost to a restart or a stuck service,
  was never evaluated until it was pushed or re-queued. Merge groups are
  discovered through their `gh-readonly-queue/...` branches
- Release-channel promotion is reconciled on the same 5-minute pass. The
  tracking branch's current commit is read from GitHub and evaluated if it
  never was (a push missed while the server was down), the in-flight
  commit is re-evaluated if its eval was lost, and the ChannelService
  re-checks the in-flight promotion (new `ChannelTask::Reconcile`).
  Previously a lost `JobsetComplete` left a promotion `Evaluating` forever,
  blocking the channel, and the pending SHA queued behind an in-flight
  promotion was lost on restart
- The aggregated change-summary check is now computed in a background task
  (one at a time, bounded to 10 minutes) instead of on the serial
  GitHubService loop. A change summary whose rebuild-impact computation
  hung (replayed from the journal on every restart) blocked every other
  GitHub task: evals could not post their jobsets or check runs, so no PR
  got evaluated until the 30-minute GitHub task timeout fired. A summary
  interrupted by a restart is now dropped rather than replayed
- Fix merge-queue evaluation, which never ran: the synthetic pull request
  built from a `merge_group` webhook lacked fields octocrab requires, so
  every `checks_requested` event was dropped with `Failed to create
  repository object`
- GitHub API requests now time out (10s connect, 60s read/write) and each
  GitHub task is bounded to 30 minutes. octocrab has no timeouts by
  default, so a single response that never arrived blocked the serial
  GitHubService, and with it every PR, indefinitely
- Journaled tasks are dropped (with an error log) after 3 replays, so a
  task that hangs or always fails no longer blocks its service on every
  startup
- Fix `get_pull_request_row` failing with `no column found for name:
  jobset_id`, which broke SHA-drift cancellation of `@eka-ci merge` and
  auto-merge. Merge eligibility now uses the head commit's jobsets

### Derivation outputs

- Fix output paths from `nix derivation show` in the versioned JSON format
  (seen with Nix 2.35), which omits the store directory: size checks,
  output references and post-build hooks received `<hash>-<name>` instead
  of `/nix/store/<hash>-<name>`, failing every size check

### NixOS module

- Raise the service's `LimitNOFILE` to 65536 (was the 1024 default soft
  limit), which the server exhausted with concurrent `nix` subprocesses

### Eval traversal

- Read evaluated drv closures with batched `nix derivation show` calls (100
  drvs per call, 4 at a time) instead of a `nix derivation show` and a
  `nix-store --query --references` per drv; dependency edges come from each
  drv's `inputDrvs`. On a 4,309-drv closure this is 45 processes instead of
  8,618, and 7.4s / 4.6s CPU instead of 71s / 968s CPU
- `batch_traverse` reads the union of all jobs' closures once instead of
  re-reading drvs shared between jobs, and lists closures with batched
  `nix-store --query --requisites`
- A batch containing an unreadable drv is split until the bad drv is
  isolated, so the rest of the batch is still recorded
- `nix-store --query --requisites` failures are now reported instead of
  being treated as an empty closure

### Shutdown

- Fix `systemctl stop`/`restart` hanging until systemd's 90s timeout and
  SIGKILL: services only checked their cancellation token between tasks, so
  an in-flight task (e.g. an eval traversing drvs with hundreds of
  `nix`/`nix-store` children) blocked shutdown. Services now drop the
  in-flight task on cancellation; journaled tasks are replayed on the next
  start
- `nix`, `nix-store`, `nix-build` and `git` subprocesses are spawned with
  `kill_on_drop`, so cancelled or timed-out work no longer leaves orphaned
  children
- Shutdown waits at most 30s for services to exit, then logs the services
  still running and aborts them

### Evaluation with iets

- Evaluate jobs with `iets eval-jobs` instead of `nix-eval-jobs` (same JSON
  lines output, with `--meta` and `--show-input-drvs`); iets evaluates the
  job file in pure mode
- `[eval] allowed_uris` (Nix `restrict-eval`) is replaced by
  `[eval] provenance`, which passes `--provenance` to add each derivation's
  source position (`pos`); an `allowed_uris` key left in `ekaci.toml` is
  ignored
- The Nix package wraps the eka-ci binaries with `nix`, `iets`, `git`,
  `bubblewrap` and `passt` on `PATH`
- Fix every evaluation aborting at startup with `per-symbol table: mmap
  failed ... Cannot allocate memory`: each iets process reserves ~36 GiB of
  address space up front, so the eval sandbox's `RLIMIT_AS` (default
  8 GiB) killed it before it read the job file. The eval sandbox no longer
  sets `RLIMIT_AS`; `[eval] memory_limit_mb` is now passed to `iets
  eval-jobs` as `--max-memory-size` (previously half of it), which replaces a
  worker whose resident memory exceeds it. Use systemd `MemoryMax` for a hard
  cap. `[checks]` still uses `RLIMIT_AS`
- Update the eval-sandbox integration tests for iets' pure mode: use a
  literal `system` instead of `builtins.currentSystem`, and expect pure-mode
  errors from the sandbox-escape attempts
- When `iets eval-jobs` fails without output, the logged error now includes
  the first `error:` line of its stderr above the last ten lines; the tail
  alone was usually `… while calling` trace frames with no error message

### Web UI

- Fix the web UI rendering a blank page: `ports.js` (WebSocket and
  token-storage ports) and `styles/custom.css` were never committed because
  `frontend/.gitignore` ignored everything in `static/` except `index.html`,
  so the deployed bundle 404'd on them and Elm never started. Both files are
  now tracked, and only the generated `static/main.js` is ignored
- Fix "Sign in with GitHub": the OAuth callback answered GitHub's browser
  redirect with raw JSON, so users landed on a JSON page instead of being
  signed in. The callback now redirects to the new `/auth/complete` page,
  passing the JWT (or an error message) in the URL fragment so it stays out
  of server and proxy logs. The GitHub-registered callback URL is unchanged
- Fix login CSRF: the OAuth `state` was generated but never checked. Login
  now stores it in a short-lived HttpOnly cookie, and the callback rejects
  any `state` that doesn't match
- The "Log in to request access" button on job pages now starts the GitHub
  login instead of opening the callback page with no code

### GitHub webhooks

- Fix every GitHub webhook being rejected with 400 ("invalid value: map,
  expected map with a single key"): the body was deserialized straight into
  octocrab's externally-tagged `WebhookEventPayload`. The event type is now
  taken from the `X-GitHub-Event` header and the payload is parsed for that
  type; a delivery without the header is rejected with 400

### Selective rebuilds (`rebuild_strategy`)

- New per-job `rebuild_strategy` in `.ekaci/config.json`: `all` (default,
  current behaviour) or `direct-referrers`, which builds only the packages a
  PR changes and the rebuilt packages that consume them directly (strict
  `inputDrvs`). Seeds are new attrs, packages whose `meta.position` file is in
  the PR's `git diff`, version changes, and roots of the rebuilt subgraph. An
  `openssl` bump builds `openssl` and its direct referrers instead of every
  transitive dependent. GitHub only
- Selection runs in the EvalService before graph traversal: only selected
  jobs enter the dependency graph. Other rebuilt jobs get `Job.skipped = 1`
  and are excluded from dispatch, check runs (eager and lazy), completion and
  failure gating, recorder hooks/size checks, and web UI / MCP progress counts
- Jobs are compared by a hash of their output paths (`Job.outputs_hash`), so
  fixed-output derivation churn does not count as a rebuild
- Falls back to building every rebuilt job, with the reason on the eval check
  run, when the base jobset is missing, was a PR head
  (`GitHubJobSets.base_sha`), predates output hashes, the eval reported no
  `inputDrvs`, or `git diff` fails
- The eval check run's output summarises the selection (built/skipped counts,
  seeds and why each was chosen)
- Fix: a PR with nothing to build (docs-only, every rebuilt job skipped, or
  every job already built) never triggered auto-merge, because only a build
  concluding a jobset did. The GitHub service now checks a PR jobset when it
  is created and sends `CheckAutoMerge` if it has already concluded
- Fix: drvs that were only evaluated, never traversed (base-commit jobs), were
  loaded into the dependency graph on restart and scheduled. `Drv.traversed`
  now marks traversed rows; eval-only rows are not loaded, and do not count as
  visited when a later traversal reaches them (they previously kept
  placeholder `is_fod` / `required_system_features` values)
- Fix: passthru.tests never triggered for worktree evaluations, because
  `meta.position` values (absolute worktree paths) were not made
  repo-relative before matching the `git diff`
- Spec: `eval_only` state, `eval_record` action and the
  `inv_eval_only_not_scheduled` invariant in `spec/ekaci.qnt`; the
  ingress routing choice is now an explicit parameter, which fixes the spec's
  tests failing on samples that took the deferred branch

### GitHub check runs

- Check-run output over GitHub's 65,535-character limit is now truncated
  instead of rejected with 422 ("Only 65535 characters are allowed").
  Coalesced gates for large groups (e.g. `texlivePackages`, ~180k chars of
  variant table) failed on every journal replay. Oversized output keeps the
  first and last 100 lines with an omission marker (or the first and last
  ~32 KB if those lines are themselves too long). This applies to every
  summary/text sent over REST and the GraphQL batcher, replacing the cap
  that only covered the change-summary check
- Gate names no longer start with a stray `"`. A jobset key containing a
  dot (core-pkgs' `buildable` set uses `"linux.v7_3"`) was stored with the
  quotes iets puts in its `attr` field, so coalescing split it into a
  `"linux` gate with `v7_3"` variants, next to the real `linux` gate. Job
  names are now the `attrPath` joined with `.` (`linux.v7_3`), so these
  variants join the `linux` gate. The first PR evaluated after upgrading
  may report these jobs as new, since the base jobset still has the
  quoted names
- Coalesced gates now say what they group: `ruby variants / Changed
  (core-pkgs)` for variants of one package, `python313Packages pkgs /
  Changed (core-pkgs)` for a package set. A group is variants when all
  its members share one `pname` (for a single changed member, when that
  pname is the group name); otherwise it is a package set. A variant
  package whose variants have differing pnames shows as `pkgs`.
  Branch protection rules that require a coalesced gate by name need
  updating

### Sandbox for evaluation and checks

- New `sandbox` workspace crate (bubblewrap + landlock via
  `ekaci-sandbox-helper`) with no unsandboxed fallback; the server refuses to
  start if the preflight fails
- Every `iets eval-jobs` run is sandboxed: cleared environment, only the store,
  the worktree and a few `/etc` files (not `/etc/nix`) visible, wall-clock and
  memory limits (`[eval]`). Not yet
  covered: the `nix eval` that lists a flake's `checks`/`packages` still runs
  unsandboxed
- Repository checks and their dev-shell capture (`nix develop` /
  `nix-shell`) run in the same sandbox: checkout read-write, `.git` and
  `/nix/store` read-only, only the dev shell's environment, limits from
  `[checks]` (`timeout_secs`, `memory_limit_mb`), replacing the previous
  check sandbox
- Network filter: evaluation, dev-shell capture and checks with
  `allow_network` get internet egress through `pasta` with private,
  link-local/metadata, CGNAT and reserved ranges and the server's own
  addresses unreachable, host loopback unreachable and IPv6 off;
  `[sandbox] network_allow`/`network_deny` adjust it. Checks without
  `allow_network` have no direct network. The Nix daemon socket stays reachable
  from every sandbox, and builds it runs (fixed-output derivations, IFD) use the
  daemon's own network, outside this filter
- `ekaci check run` is sandboxed: same sandbox on Linux (`--network-allow`,
  `--timeout`), Seatbelt on macOS, `--no-sandbox` to opt out
- NixOS module: `passt` on the service `PATH`, `/dev/net/tun` and
  `AF_NETLINK` allowed, `settings.sandbox` and `settings.checks` options

### Maintenance fixes

- Fetch from origin before creating a worktree, so commits pushed after
  the first clone (Gitea/GitLab pushes, reconstitution) can be checked out
- End option parsing (`--` / `--end-of-options`) before refs and URLs in
  git invocations; only full object ids are fetched directly as a refspec
- Accept `allow-eval-failures` as an alias of `allow_eval_failures` in
  `.ekaci/config.json` (it was silently ignored) and warn on unknown keys
- Warn at startup when `ekaci.toml` still sets the removed
  `[eval] allowed_uris`; it has had no effect since evaluation moved to
  `iets` and was silently ignored
- Docs: config path is `.ekaci/config.json`, flake mode status, GitHub App
  Contents permission, push events required for release channels
- NixOS module: drop `null` settings when generating `ekaci.toml`; put
  `nix`, `iets` and `git` on the service `PATH` (the service did
  not start: `nix config show` failed with ENOENT)
- Nix package builds again: stable Rust from fenix (nixpkgs' rustc is too
  old), `outputHashes` for the harmonia git dependency, `protobuf`, system
  openssl instead of the vendored one, and a RUNPATH fix for rust-lld.
  `nix/overlay.nix` now takes the fenix flake as an argument (direct
  importers must pass it); `overlays.default` is unchanged for consumers
- Local builders cover nix's `extra-platforms` as well as `system`, so
  derivations for e.g. `i686-linux` on an `x86_64-linux` host are scheduled
  on the local machine instead of waiting for a builder that never appears
- Output-size checks work again: sizes were looked up by store path
  instead of the stored drv path, the baseline query joined a table nothing
  writes to, and the job's `size_check` was never parsed. Baselines are now
  recorded from base-branch evaluations per job, attr path and system
  (`DrvSizeBaselines`), and a warning completes the check run as neutral
  instead of being overwritten by the success update
- Closure-size checks: the closure size (outputs plus runtime dependencies,
  shared paths counted once) of each locally built derivation is recorded
  and compared against the same base-branch baseline as the output size,
  with the same `size_check` threshold. Check-run warnings name the metric
- Size checks: optional `size_check.max_closure_increase_percent` (defaults
  to `max_increase_percent`); every exceeded size is reported on the check
  run instead of only the largest; sizes come from one async
  `nix path-info --recursive` call and are checked in a background task (at
  most 4 at once) so they no longer block the recorder
- Size baselines fall back to the pull request's base commit: when no
  base-branch baseline exists for a changed attr, the base commit's
  derivation is measured from the local store or the substituters'
  `.narinfo` files and recorded as the baseline (new
  `GitHubJobSets.base_sha`)
- Channel `forge` values are now spelled `github` and `gitlab` in
  `ekaci.toml` (previously only `git-hub`/`git-lab` parsed, so
  `forge = "github"` failed at startup); the old spellings are still
  accepted as aliases

### passthru.tests evaluation (nixpkgs-CI workflow)

- Add opt-in `passthru_tests` configuration to CI jobs that evaluates and
  builds `passthru.tests` for packages directly modified in a PR
- Filter changed packages using `meta.position` cross-referenced with
  `git diff --name-only` to distinguish direct edits from transitive
  rebuilds — only directly-modified packages trigger test evaluation
- Generate a Nix expression that safely evaluates each changed package's
  `passthru.tests` (using `builtins.tryEval` to handle missing tests
  gracefully), then feed it through the existing `iets eval-jobs` pipeline
- Test derivations flow through the standard build pipeline and appear
  as a separate `{job}/passthru-tests` jobset with individual check runs

### gRPC builder protocol

- Proto definitions for builder self-registration, build dispatch, log
  streaming, and heartbeat (`proto/ekaci/v1/builder.proto`)
- New `builder_proto` workspace crate with tonic/prost code generation
  for both client and server stubs
- `BuilderGrpcService` implementation in `server/src/grpc/` handling
  Join (registration), OpenTunnel (bidirectional build dispatch/result
  stream), StreamLog (log forwarding)
- Connected builder registry tracking builder capabilities and providing
  channels for dispatching build tasks

### NixStore trait and harmonia daemon protocol

- New `nix_store` workspace crate with `NixStore` trait abstracting store
  operations: `is_valid_path`, `query_references`, `query_requisites`,
  `query_derivation_output_map`, `query_path_info`, `store_ping`
- `SubprocessNixStore` implementation wrapping existing nix-store/nix CLI
  commands (fallback and testing)
- `DaemonNixStore` implementation using harmonia's daemon wire protocol
  with connection pooling for high-throughput store queries
- Harmonia crates (v3.3.0) added as git dependencies; verified compatible
  with the project's fenix nightly Rust toolchain
- `SchedulerService` creates and holds a shared `Arc<dyn NixStore>`
  (DaemonNixStore with CPU+1 pool size) for use by server components
- Evaluator crate re-exports `nix_store` for downstream access

### Consolidate dry-run duplication

- Move `DryRunReport` struct and `dry_run_realise` function from the server
  crate to the evaluator crate so both crates share one implementation
- Rewrite evaluator's `is_drv_cached` to use `DryRunReport::parse` instead
  of ad-hoc string matching
- Server re-exports `DryRunReport` from evaluator and provides a thin
  `dry_run_realise(&DrvId)` wrapper

### Prefetch measurement instrumentation

- Add Prometheus histograms for build phase timing: `fetch_duration_seconds`
  (spawn to first output line) and `build_duration_seconds` (first output
  to completion), labelled by platform and locality (local/remote)
- Data determines whether prefetching build inputs is worth pursuing

### Circuit-breaker for remote builders

- Track `nix-build` exit codes per remote builder and classify connection
  failures (SSH errors, connection refused/timeout, host unreachable)
- After 3 connection-related failures within 120 seconds, temporarily
  disable the builder and skip it during dispatch
- Half-open recovery: after a cooldown period, send a `nix store ping`
  probe; re-enable on success, extend backoff (up to 10 minutes) on
  failure
- Log orphan warnings when killing remote-targeting `nix-build`
  subprocesses (remote nix-daemon processes may survive)
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
