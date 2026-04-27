# Server Configuration

The server is configured via a single TOML file, by default at
`~/.config/ekaci/ekaci.toml`. This page covers the most common settings; for credential
sources see [GitHub App Setup](./github-app-setup.md) and
[Configuring Caches](./configure-caches.md).

## Minimal example

```toml
[[github_apps]]
id = "main"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }
```

## Full example

```toml
# Web server
[web]
address = "127.0.0.1"
port = 3030

# State paths
db_path  = "/var/lib/ekaci/sqlite.db"
logs_dir = "/var/log/ekaci"

# Build behaviour
build_no_output_timeout_seconds = 1200   # 20 minutes
graph_lru_capacity              = 100000 # see lru-cache-tuning.md
require_approval                = false  # require approval for external PRs

# OAuth (optional, for the web UI)
[oauth]
client_id     = "github-oauth-client-id"
client_secret = "github-oauth-client-secret"
redirect_url  = "https://your-server.com/github/auth/callback"
jwt_secret    = "your-jwt-secret"

# Security
[security]
max_hook_timeout_seconds = 300
audit_hooks              = true

# GitHub App credentials
[[github_apps]]
id = "production"
credentials = { vault = {
    address     = "https://vault.example.com:8200",
    secret_path = "eka-ci/github-app",
    token_env   = "VAULT_TOKEN"
} }

[github_apps.permissions]
allow_all     = false
allowed_repos = ["myorg/*"]

# Binary caches
[[caches]]
id           = "s3-cache"
cache_type   = "nix-copy"
destination  = "s3://bucket/path"
credentials  = { aws-secrets-manager = {
    secret_name = "eka-ci/s3-credentials",
    region      = "us-east-1"
} }

[caches.permissions]
allow_all       = false
allowed_repos   = ["myorg/production-*"]
allowed_branches = ["main"]
```

## Key settings

### `[web]`

The HTTP API and Prometheus `/metrics` endpoint bind to `address:port`. For production
deployments behind a reverse proxy, bind to `127.0.0.1` and let the proxy terminate TLS.

### `graph_lru_capacity`

Capacity of the in-memory derivation graph cache. Larger repositories need a larger cache;
see [LRU Cache Tuning](./lru-cache-tuning.md) for sizing guidance.

### `build_no_output_timeout_seconds`

A build is considered hung if it produces no output for this many seconds. The default of
20 minutes is appropriate for most Nixpkgs-style packages; bump it for repos with very slow
fixed-output derivations.

### `require_approval`

When `true`, builds for pull requests from external (non-collaborator) authors are queued
but not executed until a maintainer approves. The approval workflow is partially
implemented — see the project README for current status.

### `[security]`

`max_hook_timeout_seconds` caps the wall-clock time of any post-build hook.
`audit_hooks` enables structured audit log records every time a hook runs.

## Credentials

All credential blocks (GitHub Apps, caches, OAuth) use a tagged enum:

```toml
credentials = { env  = { vars = ["..."] } }
credentials = { file = { path = "/etc/..." } }
credentials = { vault = { address = "...", secret_path = "...", token_env = "..." } }
credentials = { aws-secrets-manager = { secret_name = "...", region = "..." } }
credentials = { systemd = { credential_id = "..." } }
credentials = { instance-metadata = { provider = "aws" } }
credentials = { aws-profile = { profile = "..." } }
credentials = { github-app-key = { app_id = "main" } }
```

Each source is documented in [GitHub App Setup](./github-app-setup.md) and
[Configuring Caches](./configure-caches.md).

## Permissions

Both `[[github_apps]]` and `[[caches]]` accept a `permissions` block:

```toml
[caches.permissions]
allow_all        = false
allowed_repos    = ["myorg/*"]
allowed_branches = ["main", "release/*"]
```

Glob patterns use `*`-style matching. When `allow_all = true`, the other lists are ignored.
