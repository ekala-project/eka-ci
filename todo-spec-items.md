# Spec-Derived Improvement Items

Issues and improvements identified by formally specifying the EkaCI build
state machine in Quint (`spec/ekaci.qnt`) and verifying invariants via
random simulation (5000 traces, 50 steps each).

---

## Critical

### 1. Interrupted builds are silently dropped

The recorder's `handle_recorder_request` has a `_ => {}` catch-all that
ignores `Interrupted(Timeout)`, `Interrupted(OOM)`, etc. Consequences:

- The drv's DB state is never updated from `Building` to `Interrupted`
- No downstream propagation occurs -- dependents wait forever
- No retry is attempted
- Only crash recovery (server restart) rescues these drvs by normalizing
  `Building` back to `Queued`

**Files**: `backend/server/src/scheduler/recorder/request_handler.rs` (line ~196)

**Fix**: Handle `Interrupted` explicitly -- update DB state, attempt retry
for transient issues (Timeout, OOM), and propagate `Blocked` to dependents
when retries are exhausted.

### 2. `Blocked` state exists but is never set

`DrvBuildState::Blocked` is defined with documentation ("at least one
transitive dependency has been interrupted") but no code path ever
transitions a drv into `Blocked`. The `Interrupted` state has no
propagation mechanism to dependents.

**Files**: `backend/shared/src/types/build_state.rs`,
`backend/server/src/db/model/build_event.rs`

**Fix**: Either implement `Blocked` propagation for interrupted builds
(analogous to `TransitiveFailure` for permanent failures), or remove the
`Blocked` variant to reduce dead code.

---

## High Priority

### 3. No retry limit for interrupted builds

`Completed(Failure)` has a clear retry budget (1 automatic retry via
`FailedRetry`). `Interrupted` builds have no retry mechanism at all --
they're silently dropped (see item 1). A build that consistently OOMs or
times out has no path to escalate to permanent failure and propagate
`TransitiveFailure`.

**Fix**: Add an interruption retry budget (e.g., 2 retries for
Timeout/OOM, 0 for Cancelled). After exhausting retries, transition to
`Completed(Failure)` and propagate transitive failures normally.

### 4. `try_send` drops during BFS cascade are silently lost

The recorder uses `try_send` for `IngressTask::CheckBuildable` during the
success cascade to avoid deadlocking on a full ingress channel. Dropped
messages are logged but not recovered. The existing "re-dispatch" mechanism
relies on the GitHub service periodically re-evaluating jobsets, which:

- Only covers GitHub-backed jobsets, not standalone or GitLab/Gitea builds
- Has a 3-minute interval, creating unnecessary latency

**Files**: `backend/server/src/scheduler/recorder/request_handler.rs`

**Fix**: Add a forge-independent fallback sweep that periodically finds
`Queued` drvs whose dependencies are all `Completed(Success)` and
re-enqueues them to ingress.

### 5. Crash recovery resets `build_attempts` to zero

`BuildGraph::from_database` normalizes `FailedRetry` -> `Queued` with
`build_attempts: 0`. A drv that already failed once gets a full 2 more
attempts after each crash. A consistently-failing drv retries indefinitely
across crashes.

**Files**: `backend/graph/src/graph.rs` (`from_database` / normalize logic)

**Fix**: Persist `build_attempts` through crash recovery. Reset only the
state (`FailedRetry` -> `Queued`), not the attempt counter.

---

## Medium Priority

### 6. Build queue not cleaned on cache-hit success

When the recorder marks a drv as `Completed(Success)` via substitution
cache hit, the drv may still be sitting in the build queue. The builder
eventually picks it up, discovers the state mismatch, and discards it --
but this wastes a build slot in the interim.

**Fix**: When recording success, cancel or remove the drv from the build
queue if present. Alternatively, have the builder thread check state before
decrementing its active-build counter.

### 7. Ingress drops non-buildable drvs without a safety net

When `ingress_process` finds a drv is not yet buildable (deps still
building), it removes it from the ingress queue and relies entirely on
`record_success` of a dependency to re-enqueue it via `CheckBuildable`.
If that `try_send` is dropped (see item 4), the drv is permanently lost
from scheduling.

**Fix**: Maintain a "pending buildability" set for drvs removed from
ingress that aren't yet buildable. Periodically sweep this set to catch
drvs whose dependencies completed but whose re-enqueue message was dropped.

### 8. `transitive_failure_map` and `failed_drvs` grow unboundedly

Failed drvs remain in `transitive_failure_map` until explicitly rebuilt
via `RebuildFailed`. For long-running CI servers processing many PRs, these
maps grow without bound.

**Fix**: Add TTL-based eviction or tie cleanup to PR lifecycle. When a PR
is closed/merged, clean up failure tracking for drvs exclusive to that
evaluation.

### 9. No detection of conflicting builder requirements in diamond deps

The `UnsatisfiableRequirements` state handles the case where no builder
has the required system features. But there's no detection of conflicting
requirements in diamond dependencies (e.g., drv A requires feature X, drv
B requires not-X, and drv C depends on both). These would fail at build
time with a confusing error rather than being caught at scheduling time.

**Fix**: During ingress buildability checks, verify that the union of
required system features across the dependency closure is satisfiable by
at least one builder configuration.
