# Optimizing GitHub API Rate Limit Usage

## Current Problem

The GitHub App installation gets 5,000 requests/hour. We're burning through this budget because:

1. **Resync is brute-force**: `resync-checks` updates all 58 check runs unconditionally, even ones already in the correct state.
2. **Jobset 1 (base branch) floods updates**: Every build completion in the 11,825-drv base branch jobset sends `UpdateBuildStatus` to the GitHub service, but most base-branch drvs don't have check runs — the `check_runs_for_drv_path` query returns empty and no API call is made, but the task still consumes queue capacity.
3. **No deduplication of UpdateBuildStatus**: If a drv's state changes multiple times (Queued→Buildable→Building→Success), each transition sends a separate `UpdateBuildStatus` task, even though only the final state matters to GitHub.
4. **Rate limiter doesn't respect HTTP 429**: The server has a 10 req/sec rate limiter, but GitHub's hourly budget is 5,000. At 10/sec sustained, we'd exhaust the budget in ~8 minutes.

## Suggestions

### 1. Conditional resync — only update stale check runs

Before updating a check run, query its current status from the GitHub API (1 request for all check runs via `GET /repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=100`), then only PATCH the ones whose GitHub state doesn't match the DB state. This turns 58 PATCH requests into 1 GET + N PATCHes (where N is the number of actually-stale ones).

**Implementation**: In `handle_resync_check_runs`, fetch all check runs for the commit in one API call, compare each against the DB state, and skip updates where they already match.

### 2. Debounce UpdateBuildStatus

Instead of sending `UpdateBuildStatus` immediately on every state transition, batch them. A drv that goes Queued→Buildable→Building→Success in 30 seconds generates 4 tasks but only the final one matters.

**Implementation**: Add a short debounce (5-10 seconds) in the recorder before sending `UpdateBuildStatus`. Use a `HashMap<DrvId, (DrvBuildState, Instant)>` that flushes entries after the debounce window. Only terminal states bypass the debounce (send immediately so the check run updates promptly).

### 3. Skip UpdateBuildStatus for drvs without check runs — earlier

Currently `notify_forge_and_channels` queries `check_runs_for_drv_path` on every build completion to check if the drv has check runs. For the 11,825-drv base branch, most drvs are intermediate dependencies with no check runs. This DB query is cheap but the task still occupies the GitHub service channel.

**Implementation**: Move the "has check runs?" check into the recorder *before* sending the `GitHubTask`, not after. This keeps the GitHub service channel clear for tasks that actually need the API.

### 4. Respect GitHub's rate limit headers

GitHub returns `X-RateLimit-Remaining` and `X-RateLimit-Reset` headers on every response. The server should read these and back off proactively when remaining < threshold (e.g., 100), rather than running into the wall and getting 403s.

**Implementation**: Wrap the octocrab client to read rate limit headers after each response. When `remaining < 100`, switch to a slower rate (1 req/5sec) or pause until reset. When `remaining == 0`, sleep until the reset timestamp.

### 5. Use GraphQL aliased mutations for batch updates

The REST Checks API has no batch endpoints, but the **GraphQL API** supports sending multiple mutations in a single request via aliases:

```graphql
mutation {
  a: updateCheckRun(input: {checkRunId: "ID1", conclusion: SUCCESS, status: COMPLETED}) { checkRun { id } }
  b: updateCheckRun(input: {checkRunId: "ID2", conclusion: SUCCESS, status: COMPLETED}) { checkRun { id } }
  c: updateCheckRun(input: {checkRunId: "ID3", conclusion: FAILURE, status: COMPLETED}) { checkRun { id } }
}
```

This costs **1 point** against the GraphQL rate limit (5,000 points/hr, separate from REST). A 58-check-run resync becomes 1-6 requests instead of 58. Even routine build status updates could be batched — accumulate completions over a 5-second window, then flush them all in one GraphQL call.

**Implementation**: Use `octocrab`'s GraphQL support or raw `reqwest` to send batched mutations. The check run node IDs needed for GraphQL can be obtained from the REST API responses (the `node_id` field) or from the initial creation response.

### 6. Cache the "has check runs" result in memory

The `check_runs_for_drv_path` DB query runs on every drv state change. Cache the result (drv → has_check_runs: bool) in a `HashSet<DrvId>` populated at jobset creation time. This eliminates repeated DB queries for the thousands of intermediate dependency drvs.

### 7. Separate GitHub service channels by priority

Use two channels: a high-priority one for check run updates (user-visible) and a low-priority one for bulk operations like resync. The high-priority channel gets processed first. This prevents a resync from starving real-time build status updates.

## Rate Limit Budget

The 5,000/hr base **scales** with installation size:
- +50 req/hr per repository beyond 20
- +50 req/hr per org member beyond 20
- Cap: 12,500 req/hr
- **Enterprise Cloud: 15,000/hr base**

The GraphQL API has a **separate** budget of 5,000 points/hr (10,000 for Enterprise Cloud). Since aliased mutations cost 1 point per request regardless of how many mutations are packed in, this is effectively unlimited for check run updates.

## Quick Win Priority

| Suggestion | Effort | Impact | Priority |
|-----------|--------|--------|----------|
| 5. GraphQL batched mutations | Medium | **Very High** | Do first — eliminates the rate limit problem entirely |
| 1. Conditional resync | Low | High | Do second — 1 GET instead of N PATCHes |
| 4. Respect rate limit headers | Medium | High | Do third — prevents hitting the wall |
| 3. Skip no-check-run drvs earlier | Low | Medium | Do fourth |
| 2. Debounce updates | Medium | Medium | Do fifth |
| 6. Cache has-check-runs | Low | Low | Nice to have |
| 7. Priority channels | Medium | Low | Nice to have |
