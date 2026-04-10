# Configuring Caches in EKA-CI

This guide explains how to configure binary caches for EKA-CI, allowing build outputs to be pushed to various cache backends.

## Overview

EKA-CI uses a **two-tier configuration model** for security:

1. **Server Configuration** (trusted): Defines available caches, credentials, and permissions
2. **Repository Configuration** (untrusted): References caches by ID only

This separation ensures that repository contributors cannot inject arbitrary commands or access credentials directly.

## Server Configuration

Cache definitions are stored in the server configuration file (typically `~/.config/ekaci/ekaci.toml` or specified via `--config-file`).

### Basic Structure

```toml
# Security settings for hook execution
[security]
max_hook_timeout_seconds = 300  # Maximum time for cache push operations
audit_hooks = true              # Enable audit logging of all cache operations

# Cache definitions
[[caches]]
id = "production-s3"
cache_type = "nix-copy"
destination = "s3://my-bucket/nix-cache?region=us-east-1"
credentials = { env = { vars = ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] } }

[[caches]]
id = "public-cachix"
cache_type = "cachix"
destination = "my-cache-name"
credentials = { cachix-token = { env_var = "CACHIX_AUTH_TOKEN" } }
```

## Cache Types

### 1. Nix Copy (S3/HTTP Binary Caches)

Uses `nix copy` to push derivations to S3-compatible storage or HTTP binary caches.

```toml
[[caches]]
id = "s3-cache"
cache_type = "nix-copy"
destination = "s3://my-bucket/nix-cache?region=us-west-2"

# Option 1: Environment variables
[caches.credentials]
env = { vars = ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] }

# Option 2: AWS profile
# [caches.credentials]
# aws-profile = { profile = "production" }

# Option 3: Credential file
# [caches.credentials]
# file = { path = "/etc/eka-ci/aws-credentials" }
```

**Supported S3 destinations:**
- `s3://bucket/path?region=REGION` - S3 with explicit region
- `s3://bucket/path?profile=PROFILE` - S3 using AWS profile
- `s3://bucket/path?endpoint=URL` - S3-compatible services (MinIO, etc.)

**HTTP binary caches:**
```toml
[[caches]]
id = "http-cache"
cache_type = "nix-copy"
destination = "https://cache.example.com"
credentials = { none = {} }  # Public cache, no auth needed
```

### 2. Cachix

Uses Cachix for binary cache storage with built-in authentication.

```toml
[[caches]]
id = "my-cachix"
cache_type = "cachix"
destination = "my-cache-name"  # Your Cachix cache name

[caches.credentials]
cachix-token = { env_var = "CACHIX_AUTH_TOKEN" }
```

**Getting a Cachix token:**
1. Sign up at [cachix.org](https://cachix.org)
2. Create a cache
3. Generate an auth token
4. Set `CACHIX_AUTH_TOKEN` environment variable when running EKA-CI

### 3. Attic

Uses Attic for self-hosted binary caches.

```toml
[[caches]]
id = "attic-cache"
cache_type = "attic"
destination = "https://attic.example.com/my-cache"

[caches.credentials]
env = { vars = ["ATTIC_TOKEN"] }
```

## Credential Sources

### Environment Variables

Read credentials from environment variables:

```toml
[caches.credentials]
env = { vars = ["VAR_NAME_1", "VAR_NAME_2"] }
```

The specified variables must be set when starting the EKA-CI server.

### File-based Credentials

Read credentials from a file:

```toml
[caches.credentials]
file = { path = "/etc/eka-ci/cache-credentials" }
```

**File format:** Key-value pairs, one per line
```
AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
```

### AWS Profile

Use credentials from `~/.aws/credentials`:

```toml
[caches.credentials]
aws-profile = { profile = "production" }
```

**AWS credentials file** (`~/.aws/credentials`):
```ini
[production]
aws_access_key_id = AKIAIOSFODNN7EXAMPLE
aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
region = us-west-2
```

### Cachix Token

Specific to Cachix authentication:

```toml
[caches.credentials]
cachix-token = { env_var = "CACHIX_AUTH_TOKEN" }
```

### No Authentication

For public caches that don't require authentication:

```toml
[caches.credentials]
none = {}
```

## Cache Permissions

Control which repositories and branches can use each cache.

### Allow All (Default)

```toml
[[caches]]
id = "public-cache"
# ... other config ...

[caches.permissions]
allow_all = true  # Any repository can use this cache
```

### Specific Repositories

```toml
[[caches]]
id = "org-cache"
# ... other config ...

[caches.permissions]
allow_all = false
allowed_repos = [
    "myorg/repo1",
    "myorg/repo2",
    "anotherorg/special-repo"
]
```

### Branch Restrictions

```toml
[[caches]]
id = "production-cache"
# ... other config ...

[caches.permissions]
allow_all = false
allowed_repos = ["myorg/myrepo"]
allowed_branches = [
    "main",           # Exact match
    "release/*",      # Prefix wildcard
    "*"               # Match all branches (if repo is allowed)
]
```

**Branch pattern syntax:**
- `main` - Exact match only
- `release/*` - Matches `release/v1.0`, `release/v2.0`, etc.
- `*/hotfix` - Matches any branch ending with `/hotfix`
- `*` - Matches all branches

## Repository Configuration

In your repository's `.eka-ci/config.json`, reference caches by ID:

```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "caches": ["production-s3", "public-cachix"]
    },
    "another-package": {
      "file": "package.nix",
      "caches": ["production-s3"]
    }
  }
}
```

**Security Note:** Repository contributors can only reference cache IDs. They cannot:
- Define arbitrary commands
- Access credentials
- Push to caches they don't have permission for
- Create new caches

## Complete Examples

### Example 1: Public Open Source Project

```toml
# Server config: ~/.config/ekaci/ekaci.toml

[security]
max_hook_timeout_seconds = 300
audit_hooks = true

[[caches]]
id = "public-cachix"
cache_type = "cachix"
destination = "my-oss-project"
credentials = { cachix-token = { env_var = "CACHIX_AUTH_TOKEN" } }
permissions = { allow_all = true }
```

```json
// Repository config: .eka-ci/config.json
{
  "jobs": {
    "stdenv": {
      "file": "default.nix",
      "caches": ["public-cachix"]
    }
  }
}
```

### Example 2: Private Company Repository

```toml
# Server config: /etc/eka-ci/ekaci.toml

[security]
max_hook_timeout_seconds = 600
audit_hooks = true

[[caches]]
id = "dev-cache"
cache_type = "nix-copy"
destination = "s3://company-dev-cache/nix?region=us-east-1"
credentials = { aws-profile = { profile = "dev" } }
permissions = { allow_all = false, allowed_repos = ["company/*"] }

[[caches]]
id = "prod-cache"
cache_type = "nix-copy"
destination = "s3://company-prod-cache/nix?region=us-east-1"
credentials = { aws-profile = { profile = "production" } }

[caches.permissions]
allow_all = false
allowed_repos = ["company/backend", "company/frontend"]
allowed_branches = ["main", "release/*"]
```

```json
// Repository config: .eka-ci/config.json
{
  "jobs": {
    "backend": {
      "file": "backend.nix",
      "caches": ["dev-cache", "prod-cache"]
    }
  }
}
```

### Example 3: Multi-Cache Strategy

Push to both a fast internal cache and a public Cachix:

```toml
[[caches]]
id = "internal-s3"
cache_type = "nix-copy"
destination = "s3://internal-cache/nix?region=us-west-2&endpoint=https://minio.internal"
credentials = { env = { vars = ["MINIO_ACCESS_KEY", "MINIO_SECRET_KEY"] } }
permissions = { allow_all = false, allowed_repos = ["myorg/*"] }

[[caches]]
id = "public-fallback"
cache_type = "cachix"
destination = "myorg-public"
credentials = { cachix-token = { env_var = "CACHIX_AUTH_TOKEN" } }
permissions = { allow_all = false, allowed_repos = ["myorg/*"] }
```

```json
{
  "jobs": {
    "my-app": {
      "file": "default.nix",
      "caches": ["internal-s3", "public-fallback"]
    }
  }
}
```

## Operational Considerations

### Setting Environment Variables

When running EKA-CI as a systemd service:

```ini
# /etc/systemd/system/eka-ci.service
[Service]
Environment="AWS_ACCESS_KEY_ID=AKIA..."
Environment="AWS_SECRET_ACCESS_KEY=wJal..."
Environment="CACHIX_AUTH_TOKEN=eyJ..."
EnvironmentFile=/etc/eka-ci/secrets.env
```

### Secrets Management

**Option 1: Environment file**
```bash
# /etc/eka-ci/secrets.env
AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
CACHIX_AUTH_TOKEN=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...
```

**Option 2: File-based credentials**
```toml
[[caches]]
id = "s3"
cache_type = "nix-copy"
destination = "s3://bucket/path"
credentials = { file = { path = "/run/secrets/aws-creds" } }
```

**Option 3: Cloud provider metadata** (for EC2/GCP/Azure)
```toml
[[caches]]
id = "s3"
cache_type = "nix-copy"
destination = "s3://bucket/path"
credentials = { none = {} }  # Uses instance metadata service
```

### Monitoring and Auditing

When `audit_hooks = true`, all cache operations are logged:

```
[INFO] Sent hook task for drv /nix/store/abc-foo.drv (job: my-package)
[WARN] Permission denied for cache 'prod-cache' in myorg/myrepo: Branch develop is not allowed
[WARN] Cache ID 'nonexistent-cache' not found in server registry, skipping
```

### Testing Cache Configuration

1. **Verify server config loads:**
   ```bash
   eka-ci-server --config-file ekaci.toml
   # Check logs for "Loading configuration file from..."
   ```

2. **Test permissions:**
   Create a test PR and check logs for permission warnings

3. **Verify credentials:**
   ```bash
   # For S3
   nix copy /nix/store/some-drv --to 's3://bucket/path?region=us-east-1'

   # For Cachix
   cachix push my-cache /nix/store/some-drv
   ```

## Troubleshooting

### Cache push fails silently

Check that:
1. Cache ID in `.eka-ci/config.json` matches server config
2. Repository has permission to use the cache
3. Credentials are valid and accessible
4. Server logs show the hook execution

### Permission denied

```
[WARN] Permission denied for cache 'prod-cache' in myorg/myrepo
```

**Solutions:**
- Add repository to `allowed_repos` list
- Check branch name matches `allowed_branches` pattern
- Set `allow_all = true` if appropriate

### Credentials not found

```
[ERROR] Failed to execute hook: Environment variable AWS_ACCESS_KEY_ID not set
```

**Solutions:**
- Ensure environment variables are set when starting server
- Check systemd service file for `Environment=` or `EnvironmentFile=`
- Verify file paths for file-based credentials

### Timeout errors

```
[WARN] Hook execution timed out after 300 seconds
```

**Solutions:**
- Increase `max_hook_timeout_seconds` in security config
- Check network connectivity to cache destination
- Verify cache backend is responsive

## Security Best Practices

1. **Use minimal permissions**: Only grant cache access to repositories that need it
2. **Separate dev/prod caches**: Use branch restrictions to prevent dev builds in production caches
3. **Rotate credentials**: Regularly rotate AWS keys and Cachix tokens
4. **Audit logs**: Monitor `audit_hooks` output for unauthorized access attempts
5. **File permissions**: Ensure credential files are readable only by the EKA-CI service user
6. **Environment isolation**: Use systemd's `PrivateTmp`, `ProtectSystem`, etc. for additional security

## Migration from Arbitrary Hooks

If you previously used arbitrary post-build hooks, migrate to the secure cache reference system:

**Before (insecure):**
```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "post_build_hooks": [{
        "name": "push-to-s3",
        "command": ["nix", "copy", "--to", "s3://bucket/path"],
        "env": {
          "AWS_ACCESS_KEY_ID": "hardcoded-key",
          "AWS_SECRET_ACCESS_KEY": "hardcoded-secret"
        }
      }]
    }
  }
}
```

**After (secure):**

Server config:
```toml
[[caches]]
id = "s3-cache"
cache_type = "nix-copy"
destination = "s3://bucket/path?region=us-east-1"
credentials = { env = { vars = ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] } }
```

Repository config:
```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "caches": ["s3-cache"]
    }
  }
}
```

## Further Reading

- [Nix Binary Caches](https://nixos.org/manual/nix/stable/package-management/binary-cache-substituter.html)
- [Cachix Documentation](https://docs.cachix.org/)
- [Attic Documentation](https://docs.attic.rs/)
- [AWS S3 Binary Caches](https://nixos.wiki/wiki/Binary_Cache)
