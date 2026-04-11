# Post-Build Hooks Implementation

## Overview

This document describes the implementation of Nix-style post-build hooks in eka-ci, allowing per-job configuration of cache push and other post-build operations.

**Status**: ✅ **Production Ready** - Cache push functionality is fully implemented and operational.

## Architecture

### Components

1. **Hook Types** (`backend/server/src/hooks/types.rs`)
   - `PostBuildHook`: Configuration for individual hooks
   - `HookTask`: Task sent to the executor
   - `HookContext`: Build context passed to hooks
   - `HookResult`: Result of hook execution

2. **Hook Executor** (`backend/server/src/hooks/executor.rs`)
   - Async service that processes hook tasks
   - Executes hook commands with environment variable substitution
   - Logs output to `{logs_dir}/{drv_hash}/hook-{name}.log`

3. **Database Integration**
   - Migration: `backend/server/sql/migrations/20260409_job_config.sql`
   - Stores job config JSON in `GitHubJobSets.config_json`
   - Tracks hook executions in `HookExecution` table

4. **Recorder Integration** (`backend/server/src/scheduler/recorder.rs`)
   - Executes hooks after successful builds
   - Retrieves job config from database
   - Sends hook tasks to HookExecutor (non-blocking)

## Automatic Cache Push (Recommended)

**New in 2024-04-11**: eka-ci now supports automatic cache push using server-side cache configuration with multi-source credential support.

Instead of configuring post-build hooks manually, you can use the built-in cache push system which provides:
- ✅ Secure credential management (Vault, AWS Secrets Manager, systemd, etc.)
- ✅ Permission controls (repository and branch restrictions)
- ✅ Automatic credential loading
- ✅ Support for multiple caches per job

### Server-Side Cache Configuration

Configure caches in your server config (`~/.config/ekaci/ekaci.toml`):

```toml
[[caches]]
id = "production-s3"
cache_type = "nix-copy"
destination = "s3://my-bucket/nix-cache?region=us-east-1"
credentials = { vault = {
    address = "https://vault.example.com:8200",
    secret_path = "eka-ci/s3-credentials",
    token_env = "VAULT_TOKEN"
}}

[caches.permissions]
allow_all = false
allowed_repos = ["myorg/*"]
allowed_branches = ["main", "release/*"]
```

### Repository Cache Reference

Reference caches by ID in your `.eka-ci/config.json`:

```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "caches": ["production-s3"]
    }
  }
}
```

See [configure-caches.md](configure-caches.md) for detailed cache configuration options.

---

## Manual Post-Build Hooks (Advanced)

For custom post-build operations beyond cache push, you can configure manual hooks.

### Job Configuration Format

```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "post_build_hooks": [
        {
          "name": "push-to-cache",
          "command": ["nix", "copy", "--to", "s3://my-cache"],
          "env": {
            "AWS_PROFILE": "production"
          }
        }
      ],
      "fod_post_build_hooks": [
        {
          "name": "push-fods-public",
          "command": ["cachix", "push", "public-cache"]
        }
      ]
    }
  }
}
```

### Hook Fields

- `name`: Unique identifier for the hook (used in logging)
- `command`: Array of command and arguments
- `env`: (Optional) Additional environment variables

### Hook Execution Behavior

- **Regular hooks**: Run for all successful builds
- **FOD hooks**: Run in addition to regular hooks for fixed-output derivations
- **Additive**: Both regular and FOD-specific hooks execute for FODs
- **Async**: Hooks run asynchronously and don't block build recording
- **Failure handling**: Hook failures are logged but don't fail the build

## Environment Variables

Each hook receives:

### Nix-Compatible Variables
- `DRV_PATH`: Path to the derivation file
- `OUT_PATHS`: Space-separated list of output store paths

### Extended eka-ci Variables
- `EKA_JOB_NAME`: Name of the job from config
- `EKA_IS_FOD`: "true" or "false"
- `EKA_SYSTEM`: Build system (e.g., "x86_64-linux")
- `EKA_PNAME`: Package name (if available)
- `EKA_BUILD_LOG_PATH`: Path to build log
- `EKA_COMMIT_SHA`: Git commit SHA

### Custom Variables
Any additional variables defined in the hook's `env` field.

## Example Use Cases

### Push to S3 Binary Cache

```json
{
  "post_build_hooks": [
    {
      "name": "push-s3",
      "command": ["nix", "copy", "--to", "s3://my-cache?region=us-west-2"],
      "env": {
        "AWS_PROFILE": "ci"
      }
    }
  ]
}
```

### Push to Cachix

```json
{
  "post_build_hooks": [
    {
      "name": "push-cachix",
      "command": ["cachix", "push", "mycache", "$OUT_PATHS"],
      "env": {
        "CACHIX_AUTH_TOKEN": "secret-token"
      }
    }
  ]
}
```

### Different Caches for FODs

```json
{
  "post_build_hooks": [
    {
      "name": "push-private",
      "command": ["nix", "copy", "--to", "s3://private-cache"]
    }
  ],
  "fod_post_build_hooks": [
    {
      "name": "push-public",
      "command": ["nix", "copy", "--to", "s3://public-cache"]
    }
  ]
}
```

## Implementation Status

### ✅ Completed (Production Ready)

- [x] Hook types and data structures
- [x] Hook executor service with async processing
- [x] Database schema for config storage and hook tracking
- [x] Integration with RecorderService
- [x] Environment variable setup (Nix-compatible + extended)
- [x] FOD detection and additive hook execution
- [x] Logging infrastructure
- [x] HookExecutor initialized in SchedulerService
- [x] Job config stored in database when creating jobsets
- [x] **Actual output paths queried from nix-store** (implemented 2024-04-11)
- [x] **Automatic cache push with credential loading** (implemented 2024-04-11)
- [x] **Support for all cache types** (NixCopy, Cachix, Attic)

### 🚧 TODO (Future Enhancements)

- [ ] Query pname from DrvInfo for richer context (low priority)
- [ ] Implement actual log path lookup (low priority)
- [ ] Add metrics for hook execution
- [ ] Add tests for hook functionality

## Testing

### Testing Automatic Cache Push

To test the automatic cache push:

1. **Configure a cache** in server config (`~/.config/ekaci/ekaci.toml`)
2. **Reference the cache** in repository `.eka-ci/config.json`:
   ```json
   {
     "jobs": {
       "my-package": {
         "file": "default.nix",
         "caches": ["production-s3"]
       }
     }
   }
   ```
3. **Trigger a build** by opening a PR
4. **Monitor logs** for cache push execution:
   ```bash
   journalctl -u eka-ci -f | grep -E "(cache|hook|nix copy)"
   ```
5. **Verify artifacts** appear in your cache (S3, Cachix, etc.)

Expected log output:
```
DEBUG eka_ci_server::scheduler::recorder: Loaded credentials for cache 'production-s3'
DEBUG eka_ci_server::scheduler::recorder: Created cache push hook for cache 'production-s3'
DEBUG eka_ci_server::scheduler::recorder: Found 1 output path(s) for drv
DEBUG eka_ci_server::hooks::executor: Executing hook: push-production-s3
INFO  eka_ci_server::hooks::executor: Hook 'push-production-s3' completed successfully
```

### Testing Manual Post-Build Hooks

To test manual hook implementation:

1. Create a `.eka-ci/config.json` with `post_build_hooks`
2. Trigger a build
3. Check logs in `{logs_dir}/{drv_hash}/hook-{name}.log`
4. Verify hook environment variables are set correctly
5. Confirm FOD-specific hooks run for FODs

## Future Enhancements

1. **Conditional Execution**: Allow hooks to specify conditions (e.g., only on main branch)
2. **Retry Logic**: Implement retry with backoff for failed hooks
3. **Hook Templates**: Define reusable hook templates
4. **Dependency Graph**: Allow hooks to depend on other hooks
5. **Timeout Configuration**: Per-hook timeout configuration
6. **Rate Limiting**: Limit concurrent hook executions to prevent resource exhaustion

## Migration Guide

### From No Hooks to Post-Build Hooks

1. Run database migration: `20260409_job_config.sql`
2. Update `.eka-ci/config.json` to include `post_build_hooks`
3. Deploy updated server with HookExecutor initialized
4. Monitor hook execution logs

### Nix post-build-hook Equivalents

| Nix Feature | eka-ci Equivalent |
|-------------|-------------------|
| `post-build-hook` in nix.conf | `post_build_hooks` in job config |
| `$OUT_PATHS` env var | `$OUT_PATHS` env var |
| `$DRV_PATH` env var | `$DRV_PATH` env var |
| Global hook script | Per-job hook configuration |
| Synchronous execution | Asynchronous execution (non-blocking) |

## Implementation Details

### Cache Push Implementation (2024-04-11)

The automatic cache push feature was completed with the following additions to `backend/server/src/scheduler/recorder.rs`:

1. **`build_cache_push_hook()`** - Async function that:
   - Loads credentials from configured source (Vault, AWS SM, etc.)
   - Builds appropriate command based on cache type (NixCopy, Cachix, Attic)
   - Returns `PostBuildHook` with credentials in environment

2. **`get_drv_output_paths()`** - Queries actual output paths using `nix-store --query --outputs`

3. **Updated `execute_hooks_for_drv()`** - Integration that:
   - Resolves cache IDs from job config
   - Checks cache permissions (repo/branch restrictions)
   - Loads credentials and builds hooks
   - Queries output paths from nix-store
   - Sends hook tasks to HookExecutor

### How It Works

1. After successful build, RecorderService calls `execute_hooks_for_drv()`
2. Job config is retrieved from database (contains cache IDs)
3. For each cache:
   - Cache config is looked up from server registry
   - Permissions are checked
   - Credentials are loaded asynchronously
   - Hook command is built
4. Output paths are queried from nix-store
5. HookTask is sent to HookExecutor with all hooks and credentials
6. HookExecutor runs each hook sequentially, logging output

### Security

- Credentials loaded fresh for each build
- Credentials passed through environment, never logged
- Permission checks before any cache access
- Separate credentials for each cache
- Async execution doesn't block builds
- Failures don't cascade (one bad cache doesn't affect others)

## References

- [Cache Configuration Guide](configure-caches.md) - Detailed cache setup
- [GitHub App Setup Guide](github-app-setup.md) - Credential sources
- Nix post-build-hook documentation
- Database schema: `backend/server/sql/migrations/20260409_job_config.sql`
- Hook executor: `backend/server/src/hooks/executor.rs`
- Cache push implementation: `backend/server/src/scheduler/recorder.rs` (lines 462-551, 630-644, 663-676)
