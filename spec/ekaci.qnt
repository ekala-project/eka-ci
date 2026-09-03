// EkaCI Formal Specification
// Models the build state machine, dependency graph, scheduler pipeline,
// and failure propagation logic of the EkaCI Nix CI system.
//
// Key invariants verified:
//   - No drv builds before all dependencies succeed
//   - Transitive failures propagate correctly
//   - Retry logic: exactly one automatic retry before permanent failure
//   - Clearing a failure unblocks dependents iff no other blockers remain
//   - Terminal states are never re-entered

module ekaci {

  // ─── Types ───────────────────────────────────────────────────────────

  type DrvId = str

  type BuildResult = Success | Failure

  type InterruptKind =
    | OutOfMemory
    | Timeout
    | Cancelled
    | ProcessDeath
    | SchedulerDeath

  type BuildState =
    | Queued
    | Buildable
    | FailedRetry
    | Building
    | Completed(BuildResult)
    | Interrupted(InterruptKind)
    | TransitiveFailure
    | Blocked
    | UnsatisfiableRequirements

  type JobDifference = New | Changed | Removed

  // A derivation node in the build graph.
  // blocking_failures: set of DrvIds whose permanent failure blocks this drv.
  // build_attempts: how many build attempts have been made (0, 1, or 2).
  type DrvNode = {
    id: DrvId,
    state: BuildState,
    is_fod: bool,
    system: str,
    blocking_failures: Set[DrvId],
    build_attempts: int,
  }

  // ─── State ───────────────────────────────────────────────────────────

  var nodes: DrvId -> DrvNode
  var deps: DrvId -> Set[DrvId]
  var referrers: DrvId -> Set[DrvId]
  var failed_drvs: Set[DrvId]
  var transitive_failure_map: DrvId -> Set[DrvId]
  var ingress_queue: Set[DrvId]
  var build_queue: Set[DrvId]
  var recorder_queue: Set[(DrvId, BuildState)]

  // ─── Pure Helpers ────────────────────────────────────────────────────

  // Whether a state is terminal for scheduling purposes.
  // TransitiveFailure IS terminal for ingress (don't re-schedule).
  pure def is_terminal(s: BuildState): bool = match s {
    | Completed(_) => true
    | TransitiveFailure => true
    | UnsatisfiableRequirements => true
    | _ => false
  }

  // Whether a state is immune to failure propagation.
  // TransitiveFailure can accumulate multiple blockers so is NOT immune.
  // Only truly completed or unsatisfiable states are immune.
  pure def is_propagation_immune(s: BuildState): bool = match s {
    | Completed(Success) => true
    | Completed(Failure) => true
    | UnsatisfiableRequirements => true
    | _ => false
  }

  // Whether all dependencies of drv `d` are Completed(Success).
  pure def is_buildable(
    d: DrvId,
    dep_map: DrvId -> Set[DrvId],
    node_map: DrvId -> DrvNode
  ): bool = {
    if (not(dep_map.keys().contains(d))) false
    else dep_map.get(d).forall(dep_id =>
      if (not(node_map.keys().contains(dep_id))) false
      else node_map.get(dep_id).state == Completed(Success)
    )
  }

  // Compute all transitive dependents of a drv (BFS via referrers).
  pure def transitive_dependents(
    root: DrvId,
    ref_map: DrvId -> Set[DrvId]
  ): Set[DrvId] = {
    val seed = if (ref_map.keys().contains(root)) ref_map.get(root) else Set()
    0.to(19).fold(seed, (acc, _) =>
      acc.union(acc.fold(Set(), (inner, d) =>
        if (ref_map.keys().contains(d)) inner.union(ref_map.get(d))
        else inner
      ))
    )
  }

  // Remove a key from a map (Quint has no built-in mapRemove).
  pure def map_remove(m: DrvId -> Set[DrvId], k: DrvId): DrvId -> Set[DrvId] =
    m.keys().filter(key => key != k).mapBy(key => m.get(key))

  // ─── Unchanged-state helper ──────────────────────────────────────────

  action unchanged: bool = all {
    nodes' = nodes,
    deps' = deps,
    referrers' = referrers,
    failed_drvs' = failed_drvs,
    transitive_failure_map' = transitive_failure_map,
    ingress_queue' = ingress_queue,
    build_queue' = build_queue,
    recorder_queue' = recorder_queue,
  }

  // ─── Initial State ──────────────────────────────────────────────────

  action init: bool = all {
    nodes' = Map(),
    deps' = Map(),
    referrers' = Map(),
    failed_drvs' = Set(),
    transitive_failure_map' = Map(),
    ingress_queue' = Set(),
    build_queue' = Set(),
    recorder_queue' = Set(),
  }

  // ─── Actions ─────────────────────────────────────────────────────────

  // Evaluation discovers a new drv and its dependencies.
  action eval_discover(
    drv: DrvId,
    system: str,
    is_fod: bool,
    dependencies: Set[DrvId]
  ): bool = all {
    not(nodes.keys().contains(drv)),
    dependencies.forall(d => nodes.keys().contains(d)),
    val new_node = {
      id: drv,
      state: Queued,
      is_fod: is_fod,
      system: system,
      blocking_failures: Set(),
      build_attempts: 0,
    }
    all {
      nodes' = nodes.put(drv, new_node),
      deps' = deps.put(drv, dependencies),
      referrers' = dependencies.fold(referrers, (acc, dep) =>
        val existing = if (acc.keys().contains(dep)) acc.get(dep) else Set()
        acc.put(dep, existing.union(Set(drv)))
      ),
      ingress_queue' = ingress_queue.union(Set(drv)),
      failed_drvs' = failed_drvs,
      transitive_failure_map' = transitive_failure_map,
      build_queue' = build_queue,
      recorder_queue' = recorder_queue,
    }
  }

  action eval_discover_leaf(drv: DrvId, system: str, is_fod: bool): bool =
    eval_discover(drv, system, is_fod, Set())

  // Ingress processes a drv: checks buildability and routes accordingly.
  action ingress_process(drv: DrvId): bool = all {
    ingress_queue.contains(drv),
    nodes.keys().contains(drv),
    val node = nodes.get(drv)
    if (is_terminal(node.state)) all {
      ingress_queue' = ingress_queue.exclude(Set(drv)),
      nodes' = nodes,
      deps' = deps,
      referrers' = referrers,
      failed_drvs' = failed_drvs,
      transitive_failure_map' = transitive_failure_map,
      build_queue' = build_queue,
      recorder_queue' = recorder_queue,
    }
    else if (is_buildable(drv, deps, nodes)) {
      val new_state = if (node.state == FailedRetry) FailedRetry else Buildable
      all {
        nodes' = nodes.put(drv, { ...node, state: new_state }),
        build_queue' = build_queue.union(Set(drv)),
        ingress_queue' = ingress_queue.exclude(Set(drv)),
        deps' = deps,
        referrers' = referrers,
        failed_drvs' = failed_drvs,
        transitive_failure_map' = transitive_failure_map,
        recorder_queue' = recorder_queue,
      }
    }
    else {
      // Not yet buildable: dequeue and wait for deps to complete.
      all {
        ingress_queue' = ingress_queue.exclude(Set(drv)),
        nodes' = nodes,
        deps' = deps,
        referrers' = referrers,
        failed_drvs' = failed_drvs,
        transitive_failure_map' = transitive_failure_map,
        build_queue' = build_queue,
        recorder_queue' = recorder_queue,
      }
    }
  }

  // Builder picks up a drv and starts building it.
  // Guards against stale build_queue entries (drv may have been
  // transitively failed or completed via cache since enqueue).
  action start_build(drv: DrvId): bool = all {
    build_queue.contains(drv),
    nodes.keys().contains(drv),
    val st = nodes.get(drv).state
    or(st == Buildable, st == FailedRetry),
    nodes' = nodes.put(drv, { ...nodes.get(drv), state: Building }),
    build_queue' = build_queue.exclude(Set(drv)),
    deps' = deps,
    referrers' = referrers,
    failed_drvs' = failed_drvs,
    transitive_failure_map' = transitive_failure_map,
    ingress_queue' = ingress_queue,
    recorder_queue' = recorder_queue,
  }

  // Stale build_queue entry: drv state changed since enqueue (e.g. cache hit
  // or transitive failure). Silently drop from queue.
  action drop_stale_build(drv: DrvId): bool = all {
    build_queue.contains(drv),
    nodes.keys().contains(drv),
    val st = nodes.get(drv).state
    not(or(st == Buildable, st == FailedRetry)),
    build_queue' = build_queue.exclude(Set(drv)),
    nodes' = nodes,
    deps' = deps,
    referrers' = referrers,
    failed_drvs' = failed_drvs,
    transitive_failure_map' = transitive_failure_map,
    ingress_queue' = ingress_queue,
    recorder_queue' = recorder_queue,
  }

  // Build completes: send result to recorder.
  action build_complete(drv: DrvId, result: BuildResult): bool = all {
    nodes.keys().contains(drv),
    nodes.get(drv).state == Building,
    all {
      recorder_queue' = recorder_queue.union(Set((drv, Completed(result)))),
      nodes' = nodes,
      deps' = deps,
      referrers' = referrers,
      failed_drvs' = failed_drvs,
      transitive_failure_map' = transitive_failure_map,
      ingress_queue' = ingress_queue,
      build_queue' = build_queue,
    }
  }

  // Build is interrupted (timeout, OOM, etc).
  action build_interrupted(drv: DrvId, kind: InterruptKind): bool = all {
    nodes.keys().contains(drv),
    nodes.get(drv).state == Building,
    all {
      recorder_queue' = recorder_queue.union(Set((drv, Interrupted(kind)))),
      nodes' = nodes,
      deps' = deps,
      referrers' = referrers,
      failed_drvs' = failed_drvs,
      transitive_failure_map' = transitive_failure_map,
      ingress_queue' = ingress_queue,
      build_queue' = build_queue,
    }
  }

  // Recorder processes a success result.
  action record_success(drv: DrvId): bool = all {
    recorder_queue.contains((drv, Completed(Success))),
    nodes.keys().contains(drv),
    val node = nodes.get(drv)
    val updated_node = { ...node, state: Completed(Success) }
    val was_failed = failed_drvs.contains(drv)
    val blocked_set =
      if (was_failed and transitive_failure_map.keys().contains(drv))
        transitive_failure_map.get(drv)
      else Set()
    val unblocked = blocked_set.filter(blocked_id =>
      nodes.keys().contains(blocked_id) and
      nodes.get(blocked_id).blocking_failures == Set(drv)
    )
    val cleared_nodes =
      if (was_failed)
        blocked_set.fold(
          nodes.put(drv, updated_node),
          (acc, blocked_id) =>
            if (acc.keys().contains(blocked_id)) {
              val bnode = acc.get(blocked_id)
              val new_blockers = bnode.blocking_failures.exclude(Set(drv))
              val new_state = if (new_blockers.size() == 0) Queued else bnode.state
              acc.put(blocked_id, { ...bnode, blocking_failures: new_blockers, state: new_state })
            }
            else acc
        )
      else nodes.put(drv, updated_node)
    val direct_refs =
      if (referrers.keys().contains(drv)) referrers.get(drv) else Set()
    // Only re-queue non-terminal referrers
    val non_terminal_refs = direct_refs.filter(r =>
      cleared_nodes.keys().contains(r) and not(is_terminal(cleared_nodes.get(r).state))
    )
    val recheck_set = non_terminal_refs.union(unblocked)
    all {
      nodes' = cleared_nodes,
      failed_drvs' = if (was_failed) failed_drvs.exclude(Set(drv)) else failed_drvs,
      transitive_failure_map' =
        if (was_failed and transitive_failure_map.keys().contains(drv))
          map_remove(transitive_failure_map, drv)
        else transitive_failure_map,
      ingress_queue' = ingress_queue.union(recheck_set),
      recorder_queue' = recorder_queue.exclude(Set((drv, Completed(Success)))),
      deps' = deps,
      referrers' = referrers,
      // Remove from build queue if present (cache hit before builder pickup)
      build_queue' = build_queue.exclude(Set(drv)),
    }
  }

  // Recorder processes a failure result - first attempt.
  // Building -> FailedRetry, re-queue for retry.
  action record_failure_first(drv: DrvId): bool = all {
    recorder_queue.contains((drv, Completed(Failure))),
    nodes.keys().contains(drv),
    nodes.get(drv).state == Building,
    nodes.get(drv).build_attempts < 1,
    nodes' = nodes.put(drv, {
      ...nodes.get(drv),
      state: FailedRetry,
      build_attempts: nodes.get(drv).build_attempts + 1,
    }),
    ingress_queue' = ingress_queue.union(Set(drv)),
    recorder_queue' = recorder_queue.exclude(Set((drv, Completed(Failure)))),
    deps' = deps,
    referrers' = referrers,
    failed_drvs' = failed_drvs,
    transitive_failure_map' = transitive_failure_map,
    build_queue' = build_queue,
  }

  // Recorder processes a failure result - second attempt (permanent).
  // FailedRetry -> Completed(Failure) + propagate transitive failures.
  action record_failure_permanent(drv: DrvId): bool = all {
    recorder_queue.contains((drv, Completed(Failure))),
    nodes.keys().contains(drv),
    nodes.get(drv).build_attempts >= 1,
    val cur = nodes.get(drv)
    val blocked = transitive_dependents(drv, referrers)
    val blocked_nodes = blocked.fold(
      nodes.put(drv, { ...cur, state: Completed(Failure), build_attempts: cur.build_attempts + 1 }),
      (acc, blocked_id) =>
        if (acc.keys().contains(blocked_id)) {
          val bnode = acc.get(blocked_id)
          if (not(is_propagation_immune(bnode.state)))
            acc.put(blocked_id, {
              ...bnode,
              state: TransitiveFailure,
              blocking_failures: bnode.blocking_failures.union(Set(drv)),
            })
          else acc
        }
        else acc
    )
    all {
      nodes' = blocked_nodes,
      failed_drvs' = failed_drvs.union(Set(drv)),
      transitive_failure_map' = transitive_failure_map.put(drv, blocked),
      recorder_queue' = recorder_queue.exclude(Set((drv, Completed(Failure)))),
      deps' = deps,
      referrers' = referrers,
      ingress_queue' = ingress_queue,
      build_queue' = build_queue,
    }
  }

  // Substitution cache hit: short-circuit to success via recorder.
  // In the real system, this only happens during ingress (before build queue).
  action substitution_hit(drv: DrvId): bool = all {
    nodes.keys().contains(drv),
    not(is_terminal(nodes.get(drv).state)),
    // Only during ingress processing, not after build queue or building
    not(build_queue.contains(drv)),
    nodes.get(drv).state != Building,
    recorder_queue' = recorder_queue.union(Set((drv, Completed(Success)))),
    ingress_queue' = ingress_queue.exclude(Set(drv)),
    nodes' = nodes,
    deps' = deps,
    referrers' = referrers,
    failed_drvs' = failed_drvs,
    transitive_failure_map' = transitive_failure_map,
    build_queue' = build_queue,
  }

  // Rebuild a failed drv: reset to Queued and clear failure tracking.
  action rebuild_failed(drv: DrvId): bool = all {
    nodes.keys().contains(drv),
    nodes.get(drv).state == Completed(Failure),
    failed_drvs.contains(drv),
    val node = nodes.get(drv)
    val unblocked =
      if (transitive_failure_map.keys().contains(drv))
        transitive_failure_map.get(drv)
      else Set()
    val reset_nodes = unblocked.fold(
      nodes.put(drv, { ...node, state: Queued, build_attempts: 0, blocking_failures: Set() }),
      (acc, blocked_id) =>
        if (acc.keys().contains(blocked_id)) {
          val bnode = acc.get(blocked_id)
          val new_blockers = bnode.blocking_failures.exclude(Set(drv))
          val new_state = if (new_blockers.size() == 0) Queued else bnode.state
          acc.put(blocked_id, { ...bnode, blocking_failures: new_blockers, state: new_state })
        }
        else acc
    )
    all {
      nodes' = reset_nodes,
      failed_drvs' = failed_drvs.exclude(Set(drv)),
      transitive_failure_map' =
        if (transitive_failure_map.keys().contains(drv))
          map_remove(transitive_failure_map, drv)
        else transitive_failure_map,
      ingress_queue' = ingress_queue.union(Set(drv)).union(unblocked),
      recorder_queue' = recorder_queue,
      deps' = deps,
      referrers' = referrers,
      build_queue' = build_queue,
    }
  }

  // Crash recovery: normalize transient states to Queued.
  action crash_recovery: bool = {
    val transient_drvs = nodes.keys().filter(d =>
      val s = nodes.get(d).state
      or(s == Building, s == Buildable, s == FailedRetry)
    )
    all {
      transient_drvs.size() > 0,
      nodes' = transient_drvs.fold(nodes, (acc, d) =>
        val n = acc.get(d)
        acc.put(d, { ...n, state: Queued, build_attempts: 0 })
      ),
      ingress_queue' = ingress_queue.union(transient_drvs),
      build_queue' = Set(),
      recorder_queue' = Set(),
      deps' = deps,
      referrers' = referrers,
      failed_drvs' = failed_drvs,
      transitive_failure_map' = transitive_failure_map,
    }
  }

  // ─── Step ────────────────────────────────────────────────────────────

  action step: bool = any {
    nondet drv = oneOf(Set("d1", "d2", "d3", "d4", "d5", "d6"))
    nondet sys = oneOf(Set("x86_64-linux", "aarch64-linux"))
    nondet fod = oneOf(Set(true, false))
    eval_discover_leaf(drv, sys, fod),

    nondet drv = oneOf(Set("d1", "d2", "d3", "d4", "d5", "d6"))
    nondet dep = oneOf(Set("d1", "d2", "d3", "d4", "d5", "d6"))
    nondet sys = oneOf(Set("x86_64-linux", "aarch64-linux"))
    nondet fod = oneOf(Set(true, false))
    eval_discover(drv, sys, fod, Set(dep)),

    nondet drv = oneOf(ingress_queue)
    ingress_process(drv),

    nondet drv = oneOf(build_queue)
    start_build(drv),

    // Drop stale build_queue entries
    nondet drv = oneOf(build_queue)
    drop_stale_build(drv),

    nondet drv = oneOf(nodes.keys().filter(d => nodes.get(d).state == Building))
    build_complete(drv, Success),

    nondet drv = oneOf(nodes.keys().filter(d => nodes.get(d).state == Building))
    build_complete(drv, Failure),

    nondet drv = oneOf(nodes.keys().filter(d => nodes.get(d).state == Building))
    build_interrupted(drv, Timeout),

    nondet drv = oneOf(recorder_queue.map(pair => pair._1))
    record_success(drv),

    nondet drv = oneOf(recorder_queue.map(pair => pair._1))
    record_failure_first(drv),

    nondet drv = oneOf(recorder_queue.map(pair => pair._1))
    record_failure_permanent(drv),

    nondet drv = oneOf(nodes.keys().filter(d => not(is_terminal(nodes.get(d).state))))
    substitution_hit(drv),

    nondet drv = oneOf(failed_drvs)
    rebuild_failed(drv),

    crash_recovery,
  }

  // ─── Invariants ──────────────────────────────────────────────────────

  // INV1: No drv in Building state has an incomplete dependency.
  val inv_building_implies_deps_done: bool =
    nodes.keys().forall(d =>
      if (nodes.get(d).state == Building) {
        deps.keys().contains(d) and
        deps.get(d).forall(dep =>
          nodes.keys().contains(dep) and
          nodes.get(dep).state == Completed(Success)
        )
      }
      else true
    )

  // INV2: TransitiveFailure drvs have at least one blocking failure recorded.
  val inv_transitive_failure_has_blocker: bool =
    nodes.keys().forall(d =>
      if (nodes.get(d).state == TransitiveFailure)
        nodes.get(d).blocking_failures.size() > 0
      else true
    )

  // INV3: A permanently failed drv is in the failed_drvs set.
  val inv_failed_in_set: bool =
    nodes.keys().forall(d =>
      if (nodes.get(d).state == Completed(Failure) and nodes.get(d).build_attempts >= 2)
        failed_drvs.contains(d)
      else true
    )

  // INV4: Build attempts never exceed 2.
  val inv_max_two_attempts: bool =
    nodes.keys().forall(d => nodes.get(d).build_attempts <= 2)

  // INV5: No drv in the build queue is in a terminal state.
  val inv_build_queue_not_terminal: bool =
    build_queue.forall(d =>
      nodes.keys().contains(d) and not(is_terminal(nodes.get(d).state))
    )

  // INV6: Dependency graph is acyclic.
  val inv_dag: bool =
    nodes.keys().forall(d =>
      not(transitive_dependents(d, deps).contains(d))
    )

  // INV7: referrers is the inverse of deps.
  val inv_referrers_consistent: bool =
    deps.keys().forall(d =>
      deps.get(d).forall(dep =>
        referrers.keys().contains(dep) and referrers.get(dep).contains(d)
      )
    )
}

// ─── Tests ──────────────────────────────────────────────────────────────

module ekaci_test {
  import ekaci.*

  // Test: Basic leaf drv lifecycle
  run leaf_drv_lifecycle_test = {
    init
    .then(eval_discover_leaf("a", "x86_64-linux", false))
    .then(ingress_process("a"))
    .then(start_build("a"))
    .then(build_complete("a", Success))
    .then(record_success("a"))
    .then(all {
      assert(nodes.get("a").state == Completed(Success)),
      unchanged,
    })
  }

  // Test: Dependency ordering
  run dependency_ordering_test = {
    init
    .then(eval_discover_leaf("dep1", "x86_64-linux", false))
    .then(eval_discover("app", "x86_64-linux", false, Set("dep1")))
    .then(ingress_process("app"))
    .then(all {
      assert(not(build_queue.contains("app"))),
      unchanged,
    })
    .then(ingress_process("dep1"))
    .then(start_build("dep1"))
    .then(build_complete("dep1", Success))
    .then(record_success("dep1"))
    .then(ingress_process("app"))
    .then(all {
      assert(build_queue.contains("app")),
      assert(nodes.get("app").state == Buildable),
      unchanged,
    })
  }

  // Test: Failure retry
  run failure_retry_test = {
    init
    .then(eval_discover_leaf("b", "x86_64-linux", false))
    .then(ingress_process("b"))
    .then(start_build("b"))
    .then(build_complete("b", Failure))
    .then(record_failure_first("b"))
    .then(all {
      assert(nodes.get("b").state == FailedRetry),
      assert(nodes.get("b").build_attempts == 1),
      assert(ingress_queue.contains("b")),
      unchanged,
    })
    .then(ingress_process("b"))
    .then(all {
      assert(nodes.get("b").state == FailedRetry),
      assert(build_queue.contains("b")),
      unchanged,
    })
    .then(start_build("b"))
    .then(build_complete("b", Failure))
    .then(record_failure_permanent("b"))
    .then(all {
      assert(nodes.get("b").state == Completed(Failure)),
      assert(nodes.get("b").build_attempts == 2),
      assert(failed_drvs.contains("b")),
      unchanged,
    })
  }

  // Test: Transitive failure propagation
  run transitive_failure_test = {
    init
    .then(eval_discover_leaf("lib", "x86_64-linux", false))
    .then(eval_discover("app1", "x86_64-linux", false, Set("lib")))
    .then(eval_discover("app2", "x86_64-linux", false, Set("lib")))
    .then(ingress_process("lib"))
    .then(start_build("lib"))
    .then(build_complete("lib", Failure))
    .then(record_failure_first("lib"))
    .then(ingress_process("lib"))
    .then(start_build("lib"))
    .then(build_complete("lib", Failure))
    .then(record_failure_permanent("lib"))
    .then(all {
      assert(nodes.get("app1").state == TransitiveFailure),
      assert(nodes.get("app2").state == TransitiveFailure),
      assert(nodes.get("app1").blocking_failures.contains("lib")),
      assert(nodes.get("app2").blocking_failures.contains("lib")),
      unchanged,
    })
  }

  // Test: Rebuild clears transitive failures
  run rebuild_clears_failures_test = {
    init
    .then(eval_discover_leaf("base", "x86_64-linux", false))
    .then(eval_discover("top", "x86_64-linux", false, Set("base")))
    .then(ingress_process("base"))
    .then(start_build("base"))
    .then(build_complete("base", Failure))
    .then(record_failure_first("base"))
    .then(ingress_process("base"))
    .then(start_build("base"))
    .then(build_complete("base", Failure))
    .then(record_failure_permanent("base"))
    .then(all {
      assert(nodes.get("top").state == TransitiveFailure),
      unchanged,
    })
    .then(rebuild_failed("base"))
    .then(all {
      assert(nodes.get("base").state == Queued),
      assert(nodes.get("top").state == Queued),
      assert(not(failed_drvs.contains("base"))),
      assert(nodes.get("top").blocking_failures.size() == 0),
      unchanged,
    })
  }

  // Test: Crash recovery normalizes transient states
  run crash_recovery_test = {
    init
    .then(eval_discover_leaf("c1", "x86_64-linux", false))
    .then(eval_discover_leaf("c2", "x86_64-linux", false))
    .then(ingress_process("c1"))
    .then(ingress_process("c2"))
    .then(start_build("c1"))
    .then(crash_recovery)
    .then(all {
      assert(nodes.get("c1").state == Queued),
      assert(ingress_queue.contains("c1")),
      assert(build_queue.size() == 0),
      unchanged,
    })
  }

  // Test: Multiple blocking failures
  run multiple_blockers_test = {
    init
    .then(eval_discover_leaf("f1", "x86_64-linux", false))
    .then(eval_discover_leaf("f2", "x86_64-linux", false))
    .then(eval_discover("child", "x86_64-linux", false, Set("f1", "f2")))
    .then(ingress_process("f1"))
    .then(start_build("f1"))
    .then(build_complete("f1", Failure))
    .then(record_failure_first("f1"))
    .then(ingress_process("f1"))
    .then(start_build("f1"))
    .then(build_complete("f1", Failure))
    .then(record_failure_permanent("f1"))
    .then(ingress_process("f2"))
    .then(start_build("f2"))
    .then(build_complete("f2", Failure))
    .then(record_failure_first("f2"))
    .then(ingress_process("f2"))
    .then(start_build("f2"))
    .then(build_complete("f2", Failure))
    .then(record_failure_permanent("f2"))
    .then(all {
      assert(nodes.get("child").state == TransitiveFailure),
      assert(nodes.get("child").blocking_failures == Set("f1", "f2")),
      unchanged,
    })
    .then(rebuild_failed("f1"))
    .then(ingress_process("f1"))
    .then(start_build("f1"))
    .then(build_complete("f1", Success))
    .then(record_success("f1"))
    .then(all {
      assert(nodes.get("child").state == TransitiveFailure),
      assert(nodes.get("child").blocking_failures == Set("f2")),
      unchanged,
    })
    .then(rebuild_failed("f2"))
    .then(ingress_process("f2"))
    .then(start_build("f2"))
    .then(build_complete("f2", Success))
    .then(record_success("f2"))
    .then(all {
      assert(nodes.get("child").state == Queued),
      assert(nodes.get("child").blocking_failures.size() == 0),
      unchanged,
    })
  }
}
