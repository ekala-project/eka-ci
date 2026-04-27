# Rebuild Detection

When a pull request changes a Nix expression, Eka CI computes which derivations need to
rebuild. This is a key input to both the [change summary](./change-summaries.md) and the
build queue.

## How it works

For each opened or updated pull request, the server:

1. Evaluates the **base** ref to produce a base jobset of derivations.
2. Evaluates the **head** ref to produce a head jobset.
3. Diffs the two jobsets, classifying each derivation as one of:
   - **Added** — present at head, missing at base.
   - **Removed** — present at base, missing at head.
   - **Rebuild** — same attribute but a different `.drv` hash.
4. For each rebuild target, walks the in-memory dependency graph to compute its
   *blast radius* — the count of transitive dependents that must also rebuild.

The resulting set of derivations is what gets enqueued for the platform-specific build
queues.

## Configuration

Rebuild detection is controlled by a small number of settings in `ekaci.toml`:

```toml
[rebuild]
# Maximum number of derivations to rebuild before classifying a PR as
# "wide" and skipping per-package builds.
max_rebuild_count = 20000

# Skip rebuild evaluation entirely for PRs that touch any of these paths.
skip_paths = ["doc/**", "**/CHANGELOG.md"]
```

The exact set of available settings is evolving; consult the source of truth at
`backend/server/src/config.rs`. Repositories can additionally constrain rebuild detection
via `change_set` rules, allowing maintainers to mark certain files as "rebuild-only" or
"docs-only" without re-evaluating Nix.

## Change sets

A *change set* is a per-repository declaration of which file globs map to which kind of
change. They are evaluated cheaply from the Git diff before a full Nix evaluation runs.
Typical uses:

- Marking `README.md` and `doc/**/*.md` as documentation-only.
- Marking `flake.lock` updates as triggering a full rebuild.
- Treating CI-only files (like `.github/**`) as no-op.

When a PR's diff is fully covered by change-set rules that imply "no rebuild", Eka CI can
skip the build phase entirely and post a "no rebuilds expected" change summary.

## Metrics

Rebuild detection emits Prometheus metrics; see [Monitoring & Metrics](./monitoring.md) for
the exact metric names. Useful series include rebuild counts per system, blast-radius
histograms, and skip-due-to-change-set counters.

## Related

- [Change Summaries](./change-summaries.md) — the user-visible output of rebuild detection.
- [LRU Cache Tuning](./lru-cache-tuning.md) — the cache that holds the dependency graph.
