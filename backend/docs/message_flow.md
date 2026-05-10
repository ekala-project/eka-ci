# EKA-CI Message Flow Architecture

## Overview

EKA-CI uses an **actor-based message passing architecture** where services communicate asynchronously via typed messages. Each service runs as an independent actor with a dedicated message queue, enabling concurrent processing and clean separation of concerns.

**Core Principles:**
- Services never call each other directly - all communication is via messages
- Messages are strongly-typed enums specific to each service
- State transitions are driven by message processing
- The in-memory dependency graph coordinates build orchestration
- Database provides persistent state, graph provides fast queries

## Service Architecture

| Service | Location | Role | Input Messages | Outputs |
|---------|----------|------|----------------|---------|
| **WebService** | `backend/server/src/web.rs` | HTTP API & webhook receiver | HTTP requests, webhooks | GitTask, GitHubTask |
| **GitService** | `backend/server/src/git/mod.rs` | Git operations coordinator | GitTask | RepoTask |
| **RepoReader** | `backend/server/src/ci/mod.rs` | CI config parser | RepoTask | EvalTask, CheckTask, GitHubTask |
| **EvalService** | `backend/server/src/nix/mod.rs` | Nix evaluation executor | EvalTask | GraphCommand, IngressTask, GitHubTask |
| **IngressService** | `backend/server/src/scheduler/ingress.rs` | Build request filter | IngressTask | BuildRequest, RecorderTask |
| **BuildQueue** | `backend/server/src/scheduler/build/` | Build executor | BuildRequest | RecorderTask |
| **RecorderService** | `backend/server/src/scheduler/recorder.rs` | Build result recorder | RecorderTask | IngressTask, GitHubTask, HookTask |
| **GraphService** | `backend/server/src/graph/` | Dependency graph (in-memory) | GraphCommand | Responses via oneshot |
| **GitHubService** | `backend/server/src/github/` | GitHub operations | GitHubTask | GitHub API calls |
| **ChecksExecutor** | `backend/server/src/services/checks.rs` | Custom check runner | CheckTask | GitHubTask |
| **HookExecutor** | `backend/server/src/hooks/` | Post-build hooks | HookTask | External cache APIs |

> **Note:** GitLab and Gitea services follow the same pattern as GitHubService but are less mature.

## Message Types

### Core Message Types

| Message Type | Purpose | Key Variants |
|--------------|---------|--------------|
| **IngressTask** | Scheduler entry point | `EvalRequest`, `CheckBuildable`, `CheckSubstitution`, `RebuildFailed` |
| **BuildRequest** | Ready-to-build derivation | Wraps `Drv` with satisfied dependencies |
| **RecorderTask** | Build completion | Contains `derivation` + `result` (Success/Failure) |
| **GitHubTask** | GitHub operations | `CreateCIGate`, `UpdateBuildStatus`, `CompleteCIGate`, ~30 variants |
| **EvalTask** | Nix evaluation trigger | `Job`, `GithubJobPR`, `TraverseDrv` |
| **GitTask** | Repository operations | `Checkout`, `GitHubCheckout` |
| **RepoTask** | CI config discovery | `Read`, `ReadGitHub` |
| **CheckTask** | Custom check execution | Contains check config + shell command |
| **GraphCommand** | Graph operations | `UpdateState`, `InsertDrvs`, `PropagateFailure`, `ClearFailure` |

### IngressTask Details

```rust
pub enum IngressTask {
    EvalRequest(Arc<DrvId>),        // New drv from eval, status unknown
    CheckBuildable(Arc<DrvId>),     // Dependency completed, recheck
    CheckSubstitution(Arc<DrvId>),  // Check if available in cache
    RebuildFailed(Arc<DrvId>),      // User-requested retry
    RebuildAllFailed,               // Retry all failed builds
}
```

**Purpose:** IngressService filters these to determine if builds are needed or can be skipped (cache hit).

### Build State Machine

```rust
pub enum DrvBuildState {
    Queued,                    // Initial state from eval
    Buildable,                 // All dependencies satisfied
    Building,                  // Build in progress
    Completed(Success/Failure),// Terminal state
    FailedRetry,              // First failure, will retry once
    TransitiveFailure,        // Blocked by failed dependency
    Interrupted,              // Build cancelled
}
```

**State Transitions:**
```
Queued → Buildable         (all deps satisfied)
Buildable → Building       (assigned to builder)
Building → Completed       (build finishes)
Building → FailedRetry     (first failure)
FailedRetry → Building     (immediate retry)
FailedRetry → Completed    (second failure, permanent)
Queued → TransitiveFailure (dependency failed)
TransitiveFailure → Queued (dependency fixed)
```

## Event Flows

### 1. GitHub PR Webhook → CI Completion

```
┌─────────────┐
│ GitHub      │ Push event / PR opened/synchronized
│ Webhook     │
└──────┬──────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ WebService (backend/server/src/github/webhook/mod.rs)      │
│ - Validate signature                                        │
│ - Check permissions & user approval                         │
│ - Store PR metadata in database                             │
└──────┬──────────────────────────────────────────────────────┘
       │
       │ GitTask::GitHubCheckout(PullRequest)
       ▼
┌─────────────────────────────────────────────────────────────┐
│ GitService (backend/server/src/git/mod.rs)                  │
│ - Clone/fetch repository                                    │
│ - Create worktree for PR head SHA                           │
└──────┬──────────────────────────────────────────────────────┘
       │
       │ RepoTask::ReadGitHub { repo_path, ci_info }
       ▼
┌─────────────────────────────────────────────────────────────┐
│ RepoReader (backend/server/src/ci/mod.rs)                   │
│ - Read .ekaci/config.json                                   │
│ - Dispatch jobs, checks, flake.checks                       │
└──────┬──────────────────────────────────────────────────────┘
       │
       ├─────────────────────────────────────┬─────────────────┐
       │                                     │                 │
       │ GitHubTask::CreateCIConfigureGate   │                 │
       │                                     │                 │
       ▼                                     ▼                 ▼
┌──────────────┐                    ┌─────────────┐  ┌────────────────┐
│ GitHubService│                    │ EvalService │  │ ChecksExecutor │
│ Create:      │                    │ (for jobs)  │  │ (for checks)   │
│ "configure"  │                    └──────┬──────┘  └────────┬───────┘
│ check run    │                           │                   │
└──────────────┘                           │                   │
       │                                   │                   │
       │ GitHubTask::CompleteCIConfigGate  │                   │
       │                                   │                   │
       │                                   ▼                   │
       │         See "Nix Evaluation Flow" below              │
       │                                   │                   │
       │                              Builds execute           │
       │                                   │                   │
       │                                   ▼                   ▼
       │         ┌───────────────────────────────────────────────┐
       │         │ All jobs & checks complete                    │
       │         │ GitHubTask::CompleteCIEvalJob                 │
       │         │ → Mark "eval / {job}" as success/failure      │
       │         └───────────────┬───────────────────────────────┘
       │                         │
       │                         ▼
       │         ┌───────────────────────────────────────────────┐
       │         │ Check auto-merge eligibility                  │
       │         │ GitHubTask::CheckAutoMerge (if enabled)       │
       │         └───────────────────────────────────────────────┘
       └───────────────────────────────────────────────────────────┘
```

### 2. Nix Evaluation Flow

```
EvalTask::GithubJobPR(eval_job, ci_info)
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ EvalService (backend/server/src/nix/mod.rs)                 │
│ - Run: nix-eval-jobs --file {file} --gc-roots-dir ...       │
│ - Parse NDJSON output stream                                │
│ - Extract derivations: { drvPath, outputs, system, ... }    │
└──────┬──────────────────────────────────────────────────────┘
       │
       ├────────────────────────┬─────────────────────────────┐
       │                        │                             │
       │ Create jobset in DB    │                             │
       ▼                        ▼                             ▼
GitHubTask::CreateJobSet    GraphCommand::InsertDrvs    IngressTask::EvalRequest
       │                        │                             │
       ▼                        ▼                             │
Create check runs for     Add nodes & edges to DAG            │
each drv:                 Track build states in memory        │
"{attr} / New|Changed"                                        │
       │                                                       │
       └───────────────────────────────────────────────────────┤
                                                               │
                          ▼────────────────────────────────────┘
                     See "Build Execution Flow" below
```

### 3. Build Execution Flow

```
IngressTask::EvalRequest(drv_id)
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ IngressService (backend/server/src/scheduler/ingress.rs)    │
│ Step 1: Check substitution (cache optimization)             │
│   Run: nix-store --realise --dry-run {drv_path}             │
└──────┬──────────────────────────────────────────────────────┘
       │
       ├─────────────────────┐
       │                     │
       │ Cache HIT           │ Cache MISS
       ▼                     ▼
RecorderTask              ┌──────────────────────────────────┐
{ drv, Completed }        │ Step 2: Check buildability       │
(skip build)              │   Query GraphService:             │
       │                  │   - All deps Completed(Success)? │
       │                  └────────┬─────────────────────────┘
       │                           │
       │                           │ Yes (buildable)
       │                           ▼
       │                  ┌──────────────────────────────────┐
       │                  │ BuildQueue assigns to builder    │
       │                  │ - Match by system (x86_64, etc)  │
       │                  │ - Separate FOD builders          │
       │                  └────────┬─────────────────────────┘
       │                           │
       │                           │ BuildRequest(drv)
       │                           ▼
       │                  ┌──────────────────────────────────┐
       │                  │ Builder Thread                   │
       │                  │ State: Buildable → Building      │
       │                  │ Run: nix-build {drv_path}        │
       │                  │ Capture logs → logs/{drv}/build  │
       │                  └────────┬─────────────────────────┘
       │                           │
       │                           │ Exit code
       │                           ▼
       │                  ┌──────────────────────────────────┐
       │                  │ RecorderTask                     │
       │                  │ { drv, Completed(Success|Fail) } │
       │                  └────────┬─────────────────────────┘
       │                           │
       └───────────────────────────┘
                                   │
                      See "Build Recording Flow" below
```

### 4. Build Recording & Propagation Flow

```
RecorderTask { derivation, result }
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ RecorderService (backend/server/src/scheduler/recorder.rs)  │
│ Step 1: Update database & graph state                       │
│   - Persist build result to Drv table                       │
│   - Update GraphService in-memory state                     │
└──────┬──────────────────────────────────────────────────────┘
       │
       ├─────────────────────────┬────────────────────────────┐
       │                         │                            │
   SUCCESS PATH             FAILURE PATH                       │
       │                         │                            │
       ▼                         ▼                            │
┌──────────────────┐   ┌─────────────────────────┐            │
│ Execute hooks    │   │ Check failure count     │            │
│ - Cachix push    │   │                         │            │
│ - Attic push     │   ├─ First failure:         │            │
│ - nix-copy       │   │   → FailedRetry         │            │
└────────┬─────────┘   │   → CheckBuildable(self)│            │
         │             │      (immediate retry)  │            │
         │             │                         │            │
         ▼             ├─ Second failure:        │            │
┌──────────────────┐   │   → Completed(Failure)  │            │
│ Clear failures   │   │   → PropagateFailure    │            │
│ GraphCommand::   │   │      (block dependents) │            │
│ ClearFailure     │   └──────────┬──────────────┘            │
│                  │              │                           │
│ Unblock any drvs │              ▼                           │
│ that were        │   ┌─────────────────────────┐            │
│ TransitiveFail   │   │ GraphCommand::          │            │
└────────┬─────────┘   │ PropagateFailure(drv)   │            │
         │             │                         │            │
         │             │ Mark all dependents as  │            │
         │             │ TransitiveFailure       │            │
         │             └──────────┬──────────────┘            │
         │                        │                           │
         ├────────────────────────┴───────────────────────────┤
         │                                                    │
         ▼                                                    │
┌─────────────────────────────────────────────────────────────┤
│ Re-queue dependents (if unblocked)                          │
│   For each dependent of this drv:                           │
│     IngressTask::CheckBuildable(dependent)                  │
└──────┬──────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ Update GitHub check runs                                    │
│   GitHubTask::UpdateBuildStatus { drv_id, status }          │
│   - in_progress → completed                                 │
│   - Set conclusion: success | failure | neutral             │
└──────┬──────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ Check jobset completion                                     │
│   If all jobs in jobset are terminal:                       │
│     - Determine overall conclusion                          │
│     - GitHubTask::CompleteCIEvalJob                         │
│     - Check auto-merge eligibility (if enabled)             │
└─────────────────────────────────────────────────────────────┘
```

### 5. Custom Checks Flow (Parallel to Builds)

```
CheckTask { check_name, owner, repo, sha, config }
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│ ChecksExecutor (backend/server/src/services/checks.rs)      │
│ - Clone repo to temp directory                              │
│ - Execute shell command (from .ekaci/config.json)           │
│   - Optional: run in nix-shell                              │
│   - Optional: allow network access                          │
│ - Capture stdout/stderr, exit code, duration                │
└──────┬──────────────────────────────────────────────────────┘
       │
       ├─────────────────────────┬────────────────────────────┐
       │                         │                            │
   Exit 0 (success)          Exit non-zero (failure)          │
       │                         │                            │
       ▼                         ▼                            │
GitHubTask::CheckComplete  GitHubTask::CheckFailed            │
       │                         │                            │
       └─────────────────────────┴────────────────────────────┤
                                                              │
                                                              ▼
                              ┌───────────────────────────────────┐
                              │ Store result in CheckResult table │
                              │ Update GitHub check run status    │
                              └───────────────────────────────────┘
```

## CI Gate Lifecycle

### Gate Types

| Gate Name | Created When | Purpose | Completion Trigger |
|-----------|--------------|---------|-------------------|
| `eka-ci / configure` | RepoReader starts | Show config parsing | All jobs/checks dispatched |
| `eka-ci / eval / {job}` | Eval starts | Show evaluation status | nix-eval-jobs completes |
| `{attr} / New\|Changed ({job})` | Per-drv after eval | Show individual build | Build completes or cache hit |
| `{check_name}` | Custom check discovered | Show check execution | Check command finishes |
| `eka-ci / approval-required` | User not approved | Block CI until approval | Manual admin approval |
| `eka-ci / change-summary` | Changes computed | Show dependency/size changes | Immediately (informational) |

### Gate State Machine

```
GitHub Check Run States:
┌─────────┐      ┌─────────────┐      ┌───────────┐
│ queued  │ ───> │ in_progress │ ───> │ completed │
└─────────┘      └─────────────┘      └───────────┘
                                            │
                                            ├─ success
                                            ├─ failure
                                            ├─ neutral
                                            ├─ cancelled
                                            ├─ action_required
                                            ├─ timed_out
                                            └─ skipped

EKA-CI Gate Transitions:
1. CreateCIConfigureGate → queued
2. (RepoReader parses .ekaci/config.json)
3. CompleteCIConfigureGate → completed (success/failure)

1. CreateCIEvalJob → queued
2. (EvalService runs nix-eval-jobs)
3. → in_progress (during evaluation)
4. CompleteCIEvalJob → completed (success)
   OR FailCIEvalJob → completed (failure)

1. CreateJobSet → creates multiple check runs (queued)
2. UpdateBuildStatus(Building) → in_progress
3. UpdateBuildStatus(Completed) → completed (success/failure/neutral)
```

## Dependency Graph Operations

### GraphService Responsibilities

The `GraphService` maintains an **in-memory directed acyclic graph (DAG)** of derivations and their dependencies. This provides O(1) lookups for build state queries, which would be expensive in SQL.

**Key Operations:**

```
InsertDrvs { drvs, refs }
  - Add nodes to graph
  - Add edges for dependencies
  - Initialize state to Queued

UpdateState { drv_id, new_state }
  - Atomic state transition
  - Used by scheduler throughout build lifecycle

PropagateFailure { failed_drv }
  - BFS traversal of dependents
  - Mark reachable nodes as TransitiveFailure
  - Record relationships in DB (DrvTransitiveFailures table)

ClearFailure { formerly_failed }
  - Query DB for blocked drvs
  - Check if all their deps are now successful
  - If yes, transition back to Queued
  - Delete from DrvTransitiveFailures

GetBuildableDrvs { }
  - Return all drvs in Buildable state
  - Used for manual rebuild triggers

ReverseReachableFromSet { seeds }
  - Compute impact of changes
  - Used for change summaries
```

### Failure Propagation Algorithm

```
When drv X fails (FailedRetry → Completed(Failure)):

1. GraphService::PropagateFailure(X)
2. Perform BFS traversal: X → dependents → transitive dependents
3. For each reachable drv Y:
     If Y.state ∈ {Queued, Buildable}:
       Y.state ← TransitiveFailure
4. Return set of blocked drvs
5. RecorderService persists:
     INSERT INTO DrvTransitiveFailures (failed_drv, blocked_drv)
```

### Success Propagation Algorithm

```
When drv X succeeds (any → Completed(Success)):

1. GraphService::ClearFailure(X)
2. Query: SELECT blocked_drv FROM DrvTransitiveFailures WHERE failed_drv = X
3. For each blocked drv Y:
     If ALL of Y's deps are Completed(Success):
       Y.state ← Queued
       Send IngressTask::CheckBuildable(Y)
4. DELETE FROM DrvTransitiveFailures WHERE failed_drv = X
```

## Message Flow Patterns

### Pattern 1: Request → Process → Record → Notify

Most build operations follow this pattern:

```
1. Request: IngressTask::EvalRequest(drv)
2. Process: IngressService checks → BuildQueue executes
3. Record:  RecorderTask persists result
4. Notify:  GitHubTask updates check runs, WebSocket broadcasts
```

### Pattern 2: Multi-Phase Gates

CI gates progress through multiple phases:

```
1. Create:   GitHubTask::CreateCIEvalJob → "queued"
2. Update:   (during processing, optional) → "in_progress"
3. Complete: GitHubTask::CompleteCIEvalJob → "completed (success/failure)"
```

### Pattern 3: Fan-out on Completion

When a build completes, it triggers multiple downstream actions:

```
RecorderTask
  ├─ Update database
  ├─ Update graph state
  ├─ Execute hooks (cache push)
  ├─ Update GitHub check runs
  ├─ Clear or propagate failures
  ├─ Re-queue dependents (CheckBuildable × N)
  ├─ Broadcast WebSocket event
  └─ Check jobset completion
```

## Improvement Notes

### Current Gaps

1. **GitLab/Gitea Support** - Webhook handlers are less mature than GitHub. Gate creation and check run logic is GitHub-centric.

2. **No Central State Validation** - State transitions are scattered across services. Consider adding a `StateValidator` to enforce legal transitions and catch bugs.

3. **Implicit Retry Logic** - `FailedRetry` state leads to retry via `CheckBuildable`, but there's no explicit "RetryBuild" message type. Could be clearer.

4. **Race Conditions** - `RecorderService` and `IngressService` run concurrently. Possible race if build completes while new eval adds same drv. GraphService sequential processing helps mitigate this.

5. **No Idempotency Tokens** - Duplicate webhook deliveries could trigger duplicate work. Consider adding idempotency keys to prevent reprocessing.

### Potential Enhancements

**Unified Forge Abstraction**
- Replace separate `GitHubTask`, `GitLabTask`, `GiteaTask` with unified `ForgeTask` enum
- Use trait-based dispatch: `ForgeAdapter::create_gate()`, `update_status()`, etc.
- Reduces code duplication, makes adding new forges easier

**Event Sourcing**
- Introduce `LifecycleEvent` enum for all state transitions
- Persist events for auditability and debugging
- Enables replaying builds, understanding failure history

**Saga Pattern for Multi-Step Workflows**
- PR check creation → eval → builds → completion is a multi-step saga
- Add explicit saga coordinator with rollback capability
- Handle partial failures gracefully

**Priority Queues**
- Not all builds are equal priority
- PRs targeting `main` could be prioritized over feature branches
- User-triggered rebuilds could be prioritized over automated ones

**Dead Letter Queue**
- If a service panics processing a message, it's lost
- Add DLQ for failed message processing
- Enables manual inspection and retry

### Code References for Deep Dives

- Webhook handling: `backend/server/src/github/webhook/mod.rs::handle_github_pr()`
- Build execution: `backend/server/src/scheduler/build/builder_thread.rs`
- State propagation: `backend/server/src/scheduler/recorder.rs::handle_recorder_request()`
- Graph operations: `backend/server/src/graph/mod.rs`
- CI config parsing: `backend/server/src/ci/mod.rs::process_github_repo_config()`
- Eval orchestration: `backend/server/src/nix/mod.rs::handle_task()`

---

**Document Version:** 2026-05-02
**Last Updated:** Initial creation based on codebase analysis
