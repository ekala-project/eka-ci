# Release Channels Operations Runbook

## Overview

Release channels provide automated continuous delivery of Nix packages to target branches when specified jobs succeed. This document describes how to configure, monitor, and troubleshoot release channels in EkaCI.

## Architecture

### Components

1. **ChannelConfig** - Admin-defined configuration in `ekaci.toml`
2. **ChannelService** - AsyncService that manages promotion evaluation
3. **ChannelPromotion table** - SQLite table tracking promotion history and in-flight evaluations
4. **GitHub Check Runs** - UI indicators showing `release/<channel>` status

### Promotion Lifecycle

1. **Push Webhook** → ChannelService receives `EvaluatePush` task
2. **Coalescing** → Latest-wins logic ensures only one in-flight evaluation per channel
3. **Evaluation** → Queries job states for `required` and `packages` lists
4. **Decision**:
   - **Ready** → All required jobs succeeded, all packages terminal → Fast-forward push
   - **Blocked** → Any required job failed → Record block reason, fire failure check run
   - **Waiting** → Jobs still building → Keep Evaluating row, wait for JobsetComplete
5. **Finalization** → Update ChannelPromotion table, fire GitHub check run

### States

- **Evaluating (0)** - In-flight evaluation, waiting for jobs to complete
- **Blocked (1)** - Required job(s) failed, cannot promote
- **Promoted (2)** - Successfully pushed to target branch
- **PushFailed (3)** - Fast-forward push failed (likely non-FF update)
- **Skipped (4)** - Superseded by newer SHA during in-flight evaluation

## Configuration

### Adding a Release Channel

Edit `ekaci.toml`:

```toml
[[channels]]
forge = "github"
owner = "myorg"
repo = "myrepo"
name = "stable"
tracking_branch = "master"
target_branch = "ekapkgs-stable"
required = ["coreutils", "bash", "gcc"]
packages = ["firefox", "chromium"]
dry_run = false
```

### Configuration Fields

| Field | Type | Description |
|-------|------|-------------|
| `forge` | String | Git forge type (`"github"`, `"gitlab"`, or `"gitea"`) |
| `owner` | String | Repository owner/organization |
| `repo` | String | Repository name |
| `name` | String | Channel name (used in check run: `release/<name>`) |
| `tracking_branch` | String | Branch to monitor for new commits |
| `target_branch` | String | Branch to fast-forward when ready |
| `required` | Array<String> | Jobs that MUST succeed for promotion |
| `packages` | Array<String> | Jobs that must reach terminal state (can fail) |
| `dry_run` | Boolean | If true, skip actual git push (testing only) |

### Validation Rules

1. **Uniqueness**: No two channels can share `(owner, repo, target_branch)`
2. **Job Names**: Must match `.eka-ci/config.json` attribute names exactly
3. **Branch Access**: GitHub App must have write permission to `target_branch`

## Monitoring

### CLI Status Query

```bash
$ eka-ci channel status stable
Channel: github:myorg/myrepo:stable

In-Flight Evaluation:
  SHA: abc123def456
  Target Branch: ekapkgs-stable
  Status: 0
  Created: 2026-05-11T10:30:00Z

Recent Promotions (5):
  • SHA: abc123def456 | Status: 2 | Created: 2026-05-11T09:00:00Z
  • SHA: def456abc123 | Status: 1 | Created: 2026-05-10T18:30:00Z
    Blocked: {"failed_required":["gcc"]}
  • SHA: 789abc012def | Status: 2 | Created: 2026-05-10T12:15:00Z
```

### GitHub Check Runs

Each channel creates a `release/<channel_name>` check run on commits:

- **In Progress** (🟡) - Evaluating
- **Success** (✅) - Promoted
- **Failure** (❌) - Blocked or PushFailed
- **Neutral** (⚪) - Skipped

### Database Queries

#### Check in-flight evaluations

```sql
SELECT channel_id, tracking_sha, target_branch, started_at
FROM ChannelPromotion
WHERE status = 0
ORDER BY started_at DESC;
```

#### Recent promotions for a channel

```sql
SELECT tracking_sha, status, started_at, completed_at, blocked_reason
FROM ChannelPromotion
WHERE channel_id = 'github:myorg/myrepo:stable'
ORDER BY started_at DESC
LIMIT 20;
```

#### Count promotions by status

```sql
SELECT status, COUNT(*) as count
FROM ChannelPromotion
WHERE channel_id LIKE 'github:myorg/myrepo:%'
GROUP BY status;
```

## Troubleshooting

### Channel Not Promoting

**Symptoms**: SHA remains in Evaluating state despite all jobs completing

**Diagnosis**:
1. Check job names match exactly:
   ```bash
   $ eka-ci channel status <channel>
   ```
2. Query job states for the SHA:
   ```sql
   SELECT j.name, d.build_state
   FROM Job j
   INNER JOIN GitHubJobSets js ON j.jobset = js.ROWID
   INNER JOIN Drv d ON j.drv_id = d.ROWID
   WHERE js.sha = '<sha>'
     AND j.name IN ('coreutils', 'bash', 'gcc');
   ```
3. Check ChannelService logs:
   ```bash
   $ journalctl -u ekaci-server | grep channel_evaluation
   ```

**Resolution**:
- Verify job names in `ekaci.toml` match `.eka-ci/config.json` exactly
- Ensure `required` jobs are actually evaluated (not missing from config)
- If stuck due to transient failure, manually finalize:
  ```sql
  UPDATE ChannelPromotion
  SET status = 2, completed_at = unixepoch()
  WHERE channel_id = '<channel_id>' AND tracking_sha = '<sha>' AND status = 0;
  ```

### Fast-Forward Push Failing (PushFailed)

**Symptoms**: Promotion reaches Ready but records PushFailed status

**Diagnosis**:
1. Check GitHub check run for error details
2. Verify target branch hasn't diverged:
   ```bash
   $ git log origin/tracking_branch..origin/target_branch
   ```

**Resolution**:
- If target branch has commits not in tracking branch, this is expected (non-FF)
- Manual merge/rebase required:
  ```bash
  $ git checkout target_branch
  $ git merge tracking_branch
  $ git push origin target_branch
  ```
- Channel will auto-promote next qualifying SHA

### Blocked Due to Flaky Test

**Symptoms**: Required job fails sporadically, blocking promotion

**Diagnosis**:
1. Query blocked promotions:
   ```sql
   SELECT tracking_sha, blocked_reason
   FROM ChannelPromotion
   WHERE channel_id = '<channel_id>' AND status = 1
   ORDER BY started_at DESC;
   ```
2. Review job logs for pattern

**Resolution**:
- **Short-term**: Manually promote blocked SHA:
  ```sql
  UPDATE ChannelPromotion
  SET status = 2, completed_at = unixepoch(), blocked_reason = NULL
  WHERE channel_id = '<channel_id>' AND tracking_sha = '<sha>';
  ```
  Then manually fast-forward:
  ```bash
  $ git push origin <sha>:refs/heads/<target_branch>
  ```
- **Long-term**: Fix flaky test or remove from `required` list

### Skipped SHAs Accumulating

**Symptoms**: Many Skipped rows in database

**Diagnosis**:
```sql
SELECT COUNT(*) as skipped_count
FROM ChannelPromotion
WHERE status = 4
  AND started_at > unixepoch() - 86400;
```

**Resolution**:
- This is normal behavior when commits arrive faster than evaluation completes
- Skipped SHAs are audit trail; they don't block new evaluations
- Optional: Prune old Skipped rows (>30 days):
  ```sql
  DELETE FROM ChannelPromotion
  WHERE status = 4
    AND started_at < unixepoch() - (30 * 86400);
  ```

## Operational Procedures

### Rolling Back a Promotion

If a promoted commit causes issues:

1. Identify the previous good SHA:
   ```bash
   $ eka-ci channel status <channel>
   ```

2. Force-push target branch to previous SHA:
   ```bash
   $ git push --force origin <previous_sha>:refs/heads/<target_branch>
   ```

3. Record the rollback in ChannelPromotion table:
   ```sql
   INSERT INTO ChannelPromotion (
     channel_id, tracking_sha, target_branch, status, started_at, completed_at
   ) VALUES (
     '<channel_id>', '<previous_sha>', '<target_branch>', 2, unixepoch(), unixepoch()
   );
   ```

### Pausing a Channel

To temporarily disable promotions without removing configuration:

1. Set dry_run=true in `ekaci.toml`:
   ```toml
   [[channels]]
   name = "stable"
   dry_run = true
   # ... rest of config
   ```

2. Restart eka-ci-server to reload config

### Re-enabling After Pause

1. Set dry_run=false in `ekaci.toml`
2. Restart eka-ci-server
3. Manually trigger evaluation of latest SHA if needed

### Migrating to New Target Branch

To change the target branch for a channel:

1. Update `target_branch` in `ekaci.toml`
2. Restart eka-ci-server
3. Next promotion will push to new branch
4. Old target branch remains at last promoted SHA

## Observability

### Key Tracing Spans

- `handle_evaluate_push` - Top-level evaluation entry point
- `snapshot_job_states` - DB queries for job states
- `perform_promotion` - Git push operation
- `handle_jobset_complete` - Jobset completion routing

### Metrics

(Not yet implemented - placeholder for future instrumentation)

- `channel_evaluations_total{status}`
- `channel_evaluation_duration_seconds`
- `channel_promotion_lag_seconds`

### Alerts

Recommended Prometheus alerts:

```yaml
- alert: ChannelStuckEvaluating
  expr: |
    max(time() - channel_evaluation_started_at{status="0"}) > 3600
  annotations:
    summary: Channel {{ $labels.channel_id }} stuck in Evaluating for >1h

- alert: ChannelPromotionFailureRate
  expr: |
    rate(channel_promotions_total{status="1"}[1h]) > 0.5
  annotations:
    summary: Channel {{ $labels.channel_id }} blocking >50% of promotions
```

## Security Considerations

1. **GitHub App Permissions**: Channels require `contents: write` on target repository
2. **Branch Protection**: Recommended to exclude target branches from branch protection rules
3. **Audit Trail**: All promotions logged in ChannelPromotion table with timestamps
4. **Fast-Forward Only**: Prevents accidental data loss (no force pushes)

## Performance

- **Coalescing**: Reduces load by skipping stale SHAs during high-frequency commits
- **DB Indexes**: `CREATE INDEX idx_channel_promotion_channel_status ON ChannelPromotion(channel_id, status)`
- **Cleanup**: Consider pruning Skipped rows older than 30 days to prevent table bloat

## References

- [Architecture Decision: Release Channels](./architecture/channels.md)
- [ChannelService Implementation](../server/src/channels/service.rs)
- [Channel Types](../server/src/channels/types.rs)
- [Database Schema](../server/schema.sql)
