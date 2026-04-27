# Eka CI Backend Architecture

## Overview

Eka CI is a **Nix-based Continuous Integration server** built in Rust using async/await with Tokio. The system follows a **multi-service actor-based architecture** where independent services communicate via message passing through Tokio's mpsc channels.

The backend is designed to efficiently build large Nix monorepos (like Nixpkgs) with intelligent scheduling, remote builder support, and comprehensive build state tracking.

## Core Design Principles

- **Service isolation**: Each major component runs as an independent service with its own message queue
- **Async-first**: Built on Tokio for high concurrency and I/O efficiency
- **Type-safe state machine**: Build states are explicitly modeled with exhaustive pattern matching
- **Dependency awareness**: Tracks derivation dependency graphs for intelligent scheduling
- **GitHub-native**: First-class integration with GitHub Pull Requests and check runs

## System Architecture

### Services Overview

The system consists of 7 core services initialized in `backend/server/src/services/mod.rs`:

1. **DbService** - SQLite database operations with connection pooling
2. **GitService** - Git repository cloning, fetching, and worktree management
3. **RepoReader** - CI configuration parsing from `.ekaci/config.json`
4. **EvalService** - Nix expression evaluation using `nix-eval-jobs`
5. **SchedulerService** - Multi-tier build orchestration (composed of 3 sub-services)
6. **GitHubService** - GitHub API integration (check runs, PR status updates via Octocrab)
7. **WebService** - HTTP API and web interface (Axum)
8. **UnixService** - Unix domain socket for CLI client communication

### Service Communication Pattern

```
┌─────────────┐
│   GitHub    │
│   Webhook   │
└──────┬──────┘
       │
       ▼
┌─────────────┐    GitTask     ┌─────────────┐
│ WebService  │───────────────▶│ GitService  │
└─────────────┘                └──────┬──────┘
                                      │ RepoTask
                                      ▼
                               ┌─────────────┐
                               │ RepoReader  │
                               └──────┬──────┘
                                      │
                        ┌─────────────┴─────────────┐
                        │                           │
                   EvalTask                    CheckTask
                        │                           │
                        ▼                           ▼
                 ┌─────────────┐           ┌──────────────┐
                 │ EvalService │           │ChecksExecutor│
                 └──────┬──────┘           └──────┬───────┘
                        │                         │
                  IngressTask                CheckResult
                        │                         │
                        ▼                         │
              ┌──────────────────┐                │
              │ IngressService   │                │
              └────────┬─────────┘                │
                       │                          │
                 BuildRequest                     │
                       │                          │
                       ▼                          │
              ┌──────────────────┐                │
              │   BuildQueue     │                │
              └────────┬─────────┘                │
                       │                          │
        ┌──────────────┼──────────────┐           │
        │              │              │           │
     [FOD]         [Local]        [Remote]        │
        │              │              │           │
        └──────────────┴──────────────┘           │
                       │                          │
                       ▼                          │
              ┌──────────────────┐                │
              │  BuilderThread   │                │
              └────────┬─────────┘                │
                       │                          │
                  NixBuild                        │
                       │                          │
                       ▼                          │
              ┌──────────────────┐                │
              │ RecorderService  │◀───────────────┘
              └────────┬─────────┘
                       │
          ┌────────────┴────────────┐
          │                         │
    Update Database          GitHubTask
          │                         │
          ▼                         ▼
    ┌──────────┐           ┌──────────────┐
    │ DbService│           │GitHubService │
    └──────────┘           └──────────────┘
```

## Data Model

### Build State Machine

The `DrvBuildState` enum (`backend/server/src/ci/mod.rs:44`) defines a comprehensive state machine:

```
Queued
  │
  ├─▶ Buildable ─────▶ Building ─────▶ Completed(Success)
  │                                 │
  │                                 └─▶ Completed(Failure)
  │                                      │
  │                                      └─▶ FailedRetry ─┐
  │                                           (1st fail)   │
  │                                                        │
  │◀───────────────────────────────────────────────────────┘
  │
  ├─▶ TransitiveFailure (dep failed, propagated)
  │
  ├─▶ Blocked (dep interrupted)
  │
  └─▶ Interrupted(kind)
       ├─ OutOfMemory
       ├─ Timeout
       ├─ Cancelled
       └─ ProcessDeath
```

**State Guarantees:**
- **Queued**: All drvs start here when discovered by evaluation
- **Buildable**: Scheduler guarantees all dependencies are successful
- **FailedRetry**: Automatic retry (one chance for transient failures)
- **TransitiveFailure**: Permanent block until upstream fixed
- **Completed**: Terminal state, never changes
- All transitions recorded in `DrvBuildEvent` with timestamps

## CI Workflow Processing

### Configuration Format

CI configurations are stored in **`.ekaci/config.json`** at the repository root:

```json
{
  "jobs": {
    "job-name": {
      "file": "/path/to/nix/file.nix",
      "allow_eval_failures": true
    }
  }
}
```

**Jobs** - Nix-based builds evaluated by `nix-eval-jobs`
**Checks** - Sandboxed imperative commands (linters, formatters, tests)

### Complete End-to-End Flow

#### 1. GitHub Trigger

```
GitHub PR opened/updated → Webhook → WebService → GitTask::GitHubCheckout
```

#### 2. Repository Checkout (`backend/server/src/services/git.rs`)

```rust
GitService:
  - Clone base repo if not exists
  - Fetch PR branch
  - Create git worktrees for base and head commits
  - Send RepoTask::ReadGitHub to RepoReader
```

Git worktrees allow parallel access to different commits without checkout conflicts.

#### 3. Configuration Parsing (`backend/server/src/services/repo_reader.rs`)

```rust
RepoReader:
  - Read .ekaci/config.json
  - Parse via CIConfig::from_str() (serde)
  - For each job:
    - Check if already processed (db.has_jobset)
    - Create EvalJob with file path
    - Send EvalTask::GithubJobPR to EvalService
  - For each check:
    - Send CheckTask to ChecksExecutor
```

#### 4. Nix Evaluation (`backend/server/src/services/eval.rs`)

```rust
EvalService:
  - Create "CI Configure Gate" GitHub check run
  - Run: nix-eval-jobs --flake .#{job.file}
  - Parse JSON stream of NixEvalDrv structs

  If evaluation fails and allow_failures=false:
    - Send GitHubTask::FailCIEvalJob
    - STOP (don't queue builds)

  - Send GitHubTask::CreateJobSet to GitHubService
  - deep_traverse() to discover all dependencies:
    - Run: nix-store --query --requisites {drv}
    - Fetch info: nix derivation show {drv}...
    - Batch insert into database (150 drvs at a time)
    - Insert dependency relationships (DrvRefs)
    - Send IngressTask::EvalRequest for each drv
```

**Optimization**: LRU cache (5000 entries) avoids re-fetching drv info.

#### 5. Build Scheduling

##### IngressService (`backend/server/src/scheduler/ingress.rs`)

```rust
IngressService receives IngressTask::EvalRequest:
  - Skip if drv in terminal state
  - Query: is_drv_buildable()
    - Check all dependencies in DrvRefs
    - Ensure all are Completed(Success)
  - If buildable:
    - Update state to Buildable
    - Send BuildRequest to BuildQueue
```

##### BuildQueue (`backend/server/src/scheduler/build/queue.rs`)

Routes builds to platform-specific queues based on `drv.system`:
- `x86_64-linux`
- `aarch64-linux`
- `x86_64-darwin`
- `aarch64-darwin`

##### PlatformQueue (`backend/server/src/scheduler/build/system_queue.rs`)

Intelligent routing based on build characteristics:

```rust
if drv.is_fod {
    → FOD Builder (dedicated for Fixed-Output Derivations)
} else if drv.prefer_local_build {
    → Local Builder
} else {
    → Remote Builder Pool or Local
}
```

**Rationale**: FOD builds (like `fetchurl`) are network-bound and benefit from dedicated capacity to avoid blocking compute-heavy builds.

#### 6. Build Execution

##### BuilderThread (`backend/server/src/scheduler/build/builder_thread.rs`)

```rust
- Manages JoinSet of concurrent builds
- Respects max_jobs limit per builder
- Spawns NixBuild task for each build
```

##### NixBuild (`backend/server/src/scheduler/build/nix_build.rs`)

Core build executor:

```rust
1. Create log file: {logs_dir}/{drv_hash}/build.log
2. Spawn: nix-build {drv_path} --builders '{config}'
3. Stream stdout/stderr to log file in real-time
4. Monitor for timeout:
   - Reset timer on each line of output
   - Kill process if no_output_timeout_seconds exceeded
5. On completion:
   - Success: Attempt to fetch substituter logs via 'nix log'
   - Failure: Return BuildOutcome::Failure
   - Timeout: Return BuildOutcome::Timeout
```

**Timeout Behavior**: The timeout is **output-based**, not total time. A build can run indefinitely as long as it produces output.

#### 7. Build Recording (`backend/server/src/scheduler/recorder.rs`)

```rust
RecorderService receives RecorderTask:
  - Update Drv.build_state in database

  On Success:
    - Clear transitive failures for this drv
    - Re-queue blocked downstream drvs
    - Send IngressTask::CheckBuildable for each downstream drvs

  On Failure (first time):
    - Update state to FailedRetry
    - Re-queue immediately (same IngressTask)

  On Failure (second time):
    - Mark as permanent Completed(Failure)
    - Propagate TransitiveFailure to all downstream drvs

  - Send GitHubTask::UpdateBuildStatus

  If all jobs in jobset concluded:
    - Determine conclusion (success if no new/changed failures)
    - Send GitHubTask::CompleteCIEvalJob
```

**Retry Logic**: Single automatic retry handles transient failures (network issues, temporary resource constraints).

#### 8. GitHub Status Updates (`backend/server/src/services/github.rs`)

```rust
GitHubService:
  - Use Octocrab (GitHub API client)
  - Create check runs (lazily on failure to reduce noise)
  - Update check run status:
    - queued → in_progress → completed
  - Set conclusion:
    - success / failure / timed_out / cancelled
  - Complete eval gate check runs when jobset finished
```

**Lazy Check Run Creation**: Only create check runs for new/changed jobs that fail. This reduces PR noise for large derivation sets where most builds are unchanged and successful.

## Scheduler Architecture Deep Dive

The scheduler is the most complex component, consisting of 3 tiers:

### Tier 1: IngressService

**Responsibility**: Determine build eligibility

```rust
// backend/server/src/scheduler/ingress.rs
IngressTask::EvalRequest { drv_path } → {
    if is_terminal_state(drv_path) {
        return; // Already built
    }

    if is_drv_buildable(drv_path) {
        update_state(drv_path, Buildable);
        send(BuildRequest { drv_path });
    }
}
```

Key query: `is_drv_buildable()` checks that **all** dependencies are `Completed(Success)`.

### Tier 2: BuildQueue

**Responsibility**: Platform routing

```rust
// backend/server/src/scheduler/build/queue.rs
BuildRequest { drv } → {
    match drv.system {
        "x86_64-linux" => x86_64_linux_queue,
        "aarch64-linux" => aarch64_linux_queue,
        "x86_64-darwin" => x86_64_darwin_queue,
        "aarch64-darwin" => aarch64_darwin_queue,
    }
}
```

### Tier 3: PlatformQueue

**Responsibility**: Builder selection and capacity management

```rust
// backend/server/src/scheduler/build/system_queue.rs
struct PlatformQueue {
    fod_builder: BuilderHandle,      // Fixed-Output Derivations
    local_builder: BuilderHandle,     // Local builds
    remote_builders: Vec<BuilderHandle>, // Remote builder pool
}

fn route_build(drv: &Drv) -> BuilderHandle {
    if drv.is_fod {
        self.fod_builder
    } else if drv.prefer_local_build {
        self.local_builder
    } else {
        // Round-robin or capacity-aware selection
        self.select_remote_builder()
    }
}
```

**Builder Types:**

- **FOD Builder**: Dedicated capacity for network-bound builds (fetchurl, fetchFromGitHub)
- **Local Builder**: Runs nix-build on the CI server itself
- **Remote Builder**: SSH-based remote Nix builders (configured via `RemoteBuilder`)

**Capacity Management**: Each builder has a `max_jobs` limit. The PlatformQueue tracks active builds per builder and queues excess work.

### Builder Health Checking

```rust
// backend/server/src/scheduler/build/builder.rs
impl Builder {
    async fn is_available(&self) -> bool {
        // Run: nix store ping --store {uri}
        Command::new("nix")
            .args(["store", "ping", "--store", &self.uri])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
```

Builders are health-checked before use to avoid queueing builds to unavailable remotes.

### Security Model

**Sandboxing via birdcage**:
- Filesystem isolation (only sees `/nix/store` and checkout directory)
- Network isolation (configurable per-check)
- No access to home directory or system files
- Prevents arbitrary file access outside checkout

**Nix Package Provisioning**:
```bash
nix-shell -p nixfmt statix --run 'env'
```

Fetches PATH and environment variables with packages available, then runs the command in that environment within the sandbox.

### Use Cases

- **Linters**: `nixfmt --check`, `statix check`
- **Tests**: `pytest`, `cargo test` (without Nix wrapping)
- **Formatters**: `prettier --check`, `black --check`
- **Custom scripts**: Any command that doesn't require a full Nix build

### Database Storage

```sql
GitHubCheckSets:
  (sha, check_name, owner, repo_name) → check_id

CheckResult:
  check_id → (success, exit_code, stdout, stderr, duration_ms, executed_at)

CheckRunInfo:
  check_id → (check_run_id, check_run_node_id)
```

Similar to job-based builds, checks integrate with GitHub check runs for PR status reporting.

## Build Features

### Remote Builders

Configured via `RemoteBuilder` struct:

```rust
struct RemoteBuilder {
    uri: String,              // ssh://user@host or nix-daemon:///
    platforms: Vec<String>,   // ["x86_64-linux", "i686-linux"]
    max_jobs: u32,            // Concurrent build limit
    speed_factor: u32,        // Priority hint (higher = prefer)
}
```

Passed to nix-build via `--builders` flag:
```bash
nix-build /nix/store/xxx.drv --builders 'ssh://builder1 x86_64-linux,i686-linux 10 1'
```

### Build Timeout Handling

**Output-based timeout** (not total duration):

```rust
// backend/server/src/scheduler/build/nix_build.rs
let no_output_timeout = Duration::from_secs(config.no_output_timeout_seconds);
let mut last_output = Instant::now();

while let Some(line) = stdout.next_line().await? {
    last_output = Instant::now(); // Reset timeout
    log_file.write_all(line.as_bytes()).await?;
}

if last_output.elapsed() > no_output_timeout {
    process.kill().await?;
    return BuildOutcome::Timeout;
}
```

**Rationale**: Large builds (like LLVM) may run for hours but should timeout if they hang (no output).

### Log Capture

**Real-time streaming**:
```rust
let log_path = format!("{}/{}/build.log", logs_dir, drv_hash);
let mut log_file = BufWriter::new(File::create(log_path).await?);

while let Some(line) = stdout.next_line().await? {
    log_file.write_all(line.as_bytes()).await?;
}

log_file.flush().await?;
```

**Post-build log fetching**:
```bash
nix log /nix/store/xxx.drv
```

For substituted builds (downloaded from cache), `nix-build` may not produce output. Attempt to fetch logs from the binary cache.

**Log serving**: WebService exposes logs via `/logs/{drv_hash}` endpoint.

### Dependency Graph Tracking

**Insertion** (`backend/server/src/services/eval.rs:deep_traverse`):
```rust
// Query all dependencies
let deps = nix_store_query_requisites(drv_path).await?;

// Batch insert relationships
db.insert_drv_refs(deps).await?;
```

**Query** (`backend/server/src/scheduler/ingress.rs:is_drv_buildable`):
```sql
SELECT COUNT(*) FROM DrvRefs
WHERE referrer = ?
  AND reference NOT IN (
    SELECT drv_path FROM Drv
    WHERE build_state = 'Completed(Success)'
  )
```

If count > 0, drv has unbuildable dependencies.

### Transitive Failure Propagation

When a drv fails permanently:

```rust
// backend/server/src/scheduler/recorder.rs
async fn propagate_transitive_failure(drv_path: &str) {
    // Find all downstream drvs
    let downstream = db.query_downstream_drvs(drv_path).await?;

    for dep_drv in downstream {
        db.insert_transitive_failure(dep_drv, drv_path).await?;
        db.update_build_state(dep_drv, TransitiveFailure).await?;
    }
}
```

**Benefits**:
- Prevents wasting resources building drvs that cannot succeed
- Clear attribution of why a build is blocked
- Can be cleared if upstream drv is fixed and rebuilt

## GitHub Integration

### OAuth Authentication

**Flow** (`backend/server/src/auth/`):
1. User clicks "Login with GitHub"
2. Redirect to GitHub OAuth authorize URL
3. GitHub redirects back with code
4. Exchange code for access token
5. Fetch user info, create JWT session token
6. Store in browser cookie

**Authorization**:
- Optional `require_approval` flag
- `ApprovedUsers` table tracks allowed GitHub usernames
- Non-approved users can view but not trigger builds

### Check Runs

**Lazy Creation Strategy**:

Only create GitHub check runs for:
- New jobs (not in base commit)
- Changed jobs (drv_path differs between base and head)
- **AND** the job failed

**Rationale**: Large repos may have thousands of derivations. Creating check runs for every successful unchanged build clutters the PR.

**Implementation** (`backend/server/src/services/github.rs:update_build_status`):
```rust
async fn update_build_status(drv: &Drv, job: &Job) {
    // Only create check run if:
    // 1. Job is New or Changed
    // 2. Build failed
    if (job.difference_type == DifferenceType::New ||
        job.difference_type == DifferenceType::Changed) &&
       drv.build_state.is_failure() {

        let check_run = octocrab
            .checks(owner, repo)
            .create_check_run(job.job_name, sha)
            .status(Status::Completed)
            .conclusion(Conclusion::Failure)
            .send()
            .await?;

        db.insert_check_run_info(drv.drv_path, check_run.id).await?;
    }
}
```

### Eval Gate Check Run

**Purpose**: Indicates whether CI configuration evaluation succeeded.

**Created for**: Every jobset (combination of commit + job_name)

**Completion**: When all drvs in the jobset reach terminal state, determine overall conclusion:
- **Success**: No new or changed drvs failed
- **Failure**: At least one new or changed drv failed

This provides a single aggregated status for the entire job.

## Monitoring and Observability

### Prometheus Metrics

**Exposed endpoint**: `/metrics`

**Metrics** (`backend/server/src/metrics.rs`):
```rust
// Build queue depth by platform
active_builds{platform="x86_64-linux"} 15
queued_builds{platform="x86_64-linux"} 42

// Process metrics (via process_collector)
process_cpu_seconds_total 1234.56
process_resident_memory_bytes 524288000
process_virtual_memory_bytes 2147483648
```

**Grafana Dashboard** (suggested):
- Build throughput (builds/minute)
- Queue depth trends
- Builder utilization
- Failure rate by job

### Structured Logging

**Libraries**: `tracing`, `tracing-subscriber`

**Log levels**:
- `ERROR`: Build failures, service panics
- `WARN`: Retry attempts, transitive failures
- `INFO`: Build starts/completions, state transitions
- `DEBUG`: Service message passing, database queries
- `TRACE`: Detailed build output

**Key spans**:
```rust
#[tracing::instrument(skip(db))]
async fn build_drv(drv_path: &str, db: &DbService) -> BuildResult {
    // Automatically logs function entry/exit with timing
}
```

### Build Logs

**Storage**: `{logs_dir}/{drv_hash}/build.log`

**Rotation**: Not implemented (logs accumulate)

**Access**:
- Web UI: View builds and their logs
- API: `GET /logs/{drv_hash}`
- CLI: `eka-cli logs {drv_hash}`

## Configuration

### Server Configuration

**Environment variables**:
```bash
DATABASE_URL=sqlite:///var/lib/eka-ci/eka.db
LOGS_DIR=/var/lib/eka-ci/logs
GITHUB_APP_ID=123456
GITHUB_APP_PRIVATE_KEY=/path/to/key.pem
BIND_ADDR=0.0.0.0:8080
```

### Builder Configuration

**Local builder**:
```rust
Builder::local(max_jobs: 40)
```

**Remote builders**:
```rust
RemoteBuilder {
    uri: "ssh://builder@10.0.0.5".to_string(),
    platforms: vec!["x86_64-linux".to_string()],
    max_jobs: 20,
    speed_factor: 1,
}
```

**FOD builder** (special local pool):
```rust
Builder::local(max_jobs: 10) // Separate capacity for FODs
```

### Timeout Configuration

```rust
no_output_timeout_seconds: 3600  // 1 hour default
```

## Graceful Shutdown

**Signal handling** (`backend/server/src/main.rs`):
```rust
let cancellation_token = CancellationToken::new();

tokio::signal::ctrl_c().await?;
cancellation_token.cancel();

// All services use run_until_cancelled()
tokio::select! {
    _ = service.run() => {}
    _ = cancellation_token.cancelled() => {}
}

// Wait for all services to drain
tokio::time::sleep(Duration::from_secs(5)).await;

// Close database pool
db.close().await;
```

**Service shutdown behavior**:
- Services drain message queues (process pending tasks)
- In-flight builds are **not** interrupted (complete naturally)
- Database writes are flushed before exit

## Performance Characteristics

### Concurrency

- **Service isolation**: Each service runs independently, maximizing CPU utilization
- **Per-builder parallelism**: Configurable `max_jobs` (default: 40 local, 10 FOD)
- **Database pooling**: SQLite connection pool with multiple readers
- **Async I/O**: Tokio runtime with work-stealing scheduler

### Scalability

**Horizontal scaling** (not implemented):
- Services could be split across processes
- Message passing via network channels (e.g., Redis pub/sub)
- Distributed builder pool

**Vertical scaling**:
- Increase `max_jobs` for builders
- Add remote builders for more capacity
- Increase database connection pool size

### Bottlenecks

1. **SQLite**: Single-writer limitation for database updates
2. **Evaluation**: `nix-eval-jobs` is CPU-bound (mitigated by caching)
3. **Local builder**: Limited by machine resources
4. **Log I/O**: Large builds produce MB of logs (mitigated by streaming)

## Security Considerations

### Checks Sandboxing

**Threat model**: Malicious `.ekaci/config.json` in untrusted PR

**Mitigations**:
- Filesystem isolation (only sees checkout + `/nix/store`)
- Network isolation (disabled by default)
- No home directory access
- No access to CI server's SSH keys, secrets, etc.

**Limitations**:
- Commands run as the CI server user (within sandbox)
- No resource limits (memory, CPU time) enforced
- DOS possible via infinite loops (timeout required)

### Nix Build Isolation

**Threat model**: Malicious Nix expressions in untrusted PR

**Mitigations**:
- Nix sandbox (enabled by default)
  - Isolated /tmp
  - No network access (except FODs)
  - Limited filesystem view
- Separate user account for Nix builds (`nixbld` group)

**Limitations**:
- Sandbox escapes may exist in Nix itself
- Resource exhaustion possible (disk, memory)
- Recommendation: Run CI server in isolated environment (container, VM)

### GitHub Authentication

**Webhook validation**:
- HMAC signature verification using GitHub App secret
- Prevents forged webhook events

**API authentication**:
- GitHub App installation tokens (short-lived)
- Scoped permissions (checks:write, contents:read)

**Session management**:
- JWT tokens with expiration
- Httponly cookies (XSS mitigation)

## Future Enhancements

### Potential Improvements

1. **Distributed architecture**: Split services across machines for scale
2. **PostgreSQL support**: Overcome SQLite concurrency limits
3. **Build caching**: Integrate with Nix binary caches (Cachix, attic)
4. **Resource limits**: Enforce memory/CPU limits via cgroups
5. **Multi-tenancy**: Support multiple organizations with isolation
6. **Build prioritization**: Prioritize PRs from approved contributors
7. **Log compression**: Gzip old logs to reduce storage
8. **Web UI improvements**: Real-time build progress, failure analysis
9. **Metrics dashboards**: Built-in Grafana/Prometheus integration
10. **Notification system**: Email/Slack alerts for build failures

### Known Limitations

1. **SQLite scaling**: Single writer becomes bottleneck at high concurrency
2. **No build artifact storage**: Relies on Nix store (garbage collected)
3. **Limited failure analysis**: No automatic error categorization
4. **Manual builder management**: No auto-scaling of remote builders
5. **No incremental evaluation**: Re-evaluates entire job on every commit

## References

### Key Files

- Service initialization: `backend/server/src/services/mod.rs`
- CI configuration: `backend/server/src/ci/config.rs`
- Build state machine: `backend/server/src/ci/mod.rs`
- Scheduler tiers: `backend/server/src/scheduler/`
- Nix build execution: `backend/server/src/scheduler/build/nix_build.rs`
- Checks executor: `backend/server/src/checks/executor.rs`
- GitHub integration: `backend/server/src/services/github.rs`
- Database migrations: `backend/server/sql/migrations/`

### External Dependencies

- **Tokio**: Async runtime
- **Axum**: Web framework
- **SQLx**: Async SQL with compile-time query checking
- **Octocrab**: GitHub API client
- **Serde**: JSON serialization
- **Birdcage**: Sandboxing via Linux namespaces
- **nix-eval-jobs**: Parallel Nix evaluation (external tool)
- **Nix**: Build execution and derivation management

---

**Last Updated**: 2026-02-13
**Nix Version**: 2.x compatible
**Rust Edition**: 2021
