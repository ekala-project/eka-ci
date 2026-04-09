# Post-Build Hooks Implementation

## Overview

This document describes the implementation of Nix-style post-build hooks in eka-ci, allowing per-job configuration of cache push and other post-build operations.

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

## Configuration

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

### Completed

- [x] Hook types and data structures
- [x] Hook executor service with async processing
- [x] Database schema for config storage and hook tracking
- [x] Integration with RecorderService
- [x] Environment variable setup (Nix-compatible + extended)
- [x] FOD detection and additive hook execution
- [x] Logging infrastructure

### TODO

- [ ] Initialize HookExecutor in main service startup
- [ ] Store job config in database when creating jobsets
- [ ] Query actual output paths from nix (currently uses drv path)
- [ ] Query pname from DrvInfo for richer context
- [ ] Implement actual log path lookup
- [ ] Add metrics for hook execution
- [ ] Add tests for hook functionality
- [ ] Add WebSocket events for hook status

## Testing

To test the implementation:

1. Create a `.eka-ci/config.json` with post-build hooks
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

## References

- Nix post-build-hook documentation
- eka-ci configuration schema
- Database schema: `backend/server/sql/migrations/20260409_job_config.sql`
- Hook executor implementation: `backend/server/src/hooks/executor.rs`
