# EkaCI

Continuous Integration server for Nix projects with advanced caching, security, and GitHub integration.

This tool provides an optimized reviewing experience for small to large Nix package repositories. In particular, the tool provides:
- Succinct PR review workflows
  - Does the eval still succeed?
  - Only inspect builds that have changed
    - Added, removed, [newly/still] succeeding, [newly/still] failing builds
  - Closure size difference
  - Retained dependency differences
  - Captured logs
  - Explore dependency failures (similar to Hydra)
  - (Stretch goal) Diffoscope-like diff of package outputs
  - (Stretch goal) Diffs of NixOS configurations

Ultimately, this tool is meant to answer, "should I merge this PR?" in the quickest manner possible.
Curating a Nix repository should not be highly limited to manual review processes of a reviewer.
This doesn't scale well, and is error prone.

## Features

### ✅ Core CI Functionality (Production Ready)

- **GitHub App Integration**
  - Webhook-based event handling
  - Pull request check runs with status updates
  - Installation and repository tracking
  - Merge queue support
  - Multi-source credential management (8 different sources)
  - Fine-grained permission controls

- **Nix Build System**
  - Dependency graph tracking with LRU caching (100k nodes default)
  - Intelligent build queue with multi-tier scheduling
  - Platform-specific build queues (x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin)
  - Dedicated FOD (Fixed-Output Derivation) queue
  - Remote builder support via SSH
  - System features support (respects `requiredSystemFeatures`)
  - Build retry logic with transitive failure propagation
  - Build timeout handling (output-based)

- **Binary Cache Integration**
  - Multiple backend support: S3, Cachix, Attic
  - Multi-source credential support:
    - Environment variables
    - File-based credentials (JSON or key=value)
    - HashiCorp Vault
    - AWS Secrets Manager
    - systemd credentials (TPM2-encrypted)
    - Instance metadata (EC2/GCP/Azure)
    - AWS profiles
    - GitHub App key files
  - Cache permission system (repository and branch restrictions)
  - Glob pattern matching for flexible access control

- **Security Architecture**
  - Credential isolation (server vs repository configuration)
  - Sandboxed check execution (via birdcage)
  - Network and filesystem isolation for checks
  - Audit logging support
  - No credentials exposed to repository configs

- **Monitoring & Observability**
  - Prometheus metrics endpoint
  - Build queue metrics
  - Graph cache utilization metrics
  - WebSocket support for real-time updates
  - Structured logging (tracing)
  - Build log capture and storage

### 🚧 Partially Implemented

- **Post-Build Hooks**
  - Framework implemented
  - Database schema and executor service ready
  - Integration with main service pending
  - See: [docs/post-build-hooks.md](docs/post-build-hooks.md)

- **Approval Workflow**
  - Basic approval checking implemented
  - `--require-approval` flag supported
  - OAuth authentication infrastructure ready
  - Full workflow UI pending

- **Web Interface**
  - HTTP API endpoints functional
  - WebSocket real-time updates working
  - Log viewing available
  - Full PR review portal UI pending

### 📋 Planned Features

- **Automated Cache Pushing**
  - Push successful builds to configured caches
  - Integration with existing cache infrastructure

- **Enhanced Metrics**
  - Closure size calculations
  - Dependency comparison between branches
  - Metric visualization in web UI

- **PR Review Portal**
  - Ordered list of PRs (by rebuild count, lines changed)
  - Textual diff viewing
  - Metrics dashboard
  - Approval and merge capabilities

- **Security Enhancements**
  - Webhook signature verification (framework ready)
  - Additional audit logging features

- **Future Evaluation Modes**
  - "OfBorg" convention (read attr path from commit message)
  - Flake checks mode (similar to Garnix)
  - Flake develop actions (impure command execution)

## Design

See the [design document](./DESIGN.md) for details about a high-level overview of EkaCI.

See the [architecture document](./docs/ARCHITECTURE.md) for details about EkaCI's current implementation.

## Quick Start

### Prerequisites

- Nix package manager installed
- GitHub organization with admin access
- Publicly accessible server (for webhooks)

### 1. Create GitHub App

Follow the comprehensive guide: [docs/github-app-setup.md](docs/github-app-setup.md)

Quick summary:
1. Create GitHub App at `https://github.com/organizations/YOUR_ORG/settings/apps`
2. Configure permissions: Checks (RW), Contents (R), Pull Requests (R)
3. Subscribe to events: pull_request, workflow_run, merge_group, installation
4. Generate and download private key

### 2. Configure Server

Create `~/.config/ekaci/ekaci.toml`:

```toml
# GitHub App credentials (choose one method)
[[github_apps]]
id = "main"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }

# Optional: Cache configuration
[[caches]]
id = "production-s3"
cache_type = "nix-copy"
destination = "s3://my-bucket/nix-cache?region=us-east-1"
credentials = { env = { vars = ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"] } }

[caches.permissions]
allow_all = false
allowed_repos = ["myorg/*"]
allowed_branches = ["main", "release/*"]
```

See [docs/github-app-setup.md](docs/github-app-setup.md) for detailed credential configuration options.

See [docs/configure-caches.md](docs/configure-caches.md) for cache configuration details.

### 3. Configure Repository

Add `.eka-ci/config.json` to your repository:

```json
{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "allow_eval_failures": true,
      "caches": ["production-s3"]
    }
  },
  "checks": {
    "nixfmt": {
      "shell": "formatting",
      "command": "nixfmt --check **/*.nix",
      "allow_network": false
    }
  }
}
```

### 4. Run Server

```bash
nix build
./result/bin/eka-ci-server
```

Or with systemd:

```ini
[Unit]
Description=eka-ci server
After=network.target

[Service]
Type=simple
ExecStart=/path/to/eka-ci-server
Restart=on-failure
User=eka-ci
Environment="VAULT_TOKEN=s.your-token"

[Install]
WantedBy=multi-user.target
```

## Configuration Reference

### Server Configuration

Full configuration options (`~/.config/ekaci/ekaci.toml`):

```toml
# Web server settings
[web]
address = "127.0.0.1"
port = 3030

# Database and logs
db_path = "/var/lib/ekaci/sqlite.db"
logs_dir = "/var/log/ekaci"

# Build configuration
build_no_output_timeout_seconds = 1200  # 20 minutes
graph_lru_capacity = 100000  # LRU cache size
require_approval = false  # Require approval for external PRs

# OAuth (optional, for web UI)
[oauth]
client_id = "github-oauth-client-id"
client_secret = "github-oauth-client-secret"
redirect_url = "https://your-server.com/github/auth/callback"
jwt_secret = "your-jwt-secret"

# Security
[security]
max_hook_timeout_seconds = 300
audit_hooks = true

# GitHub App credentials
[[github_apps]]
id = "production"
credentials = { vault = {
    address = "https://vault.example.com:8200",
    secret_path = "eka-ci/github-app",
    token_env = "VAULT_TOKEN"
}}

[github_apps.permissions]
allow_all = false
allowed_repos = ["myorg/*"]

# Cache configurations
[[caches]]
id = "s3-cache"
cache_type = "nix-copy"
destination = "s3://bucket/path"
credentials = { aws-secrets-manager = {
    secret_name = "eka-ci/s3-credentials",
    region = "us-east-1"
}}

[caches.permissions]
allow_all = false
allowed_repos = ["myorg/production-*"]
allowed_branches = ["main"]
```

### Repository Configuration

Repository-specific settings (`.eka-ci/config.json`):

```json
{
  "jobs": {
    "package-name": {
      "file": "path/to/file.nix",
      "attr_path": "optional.attr.path",
      "allow_eval_failures": false,
      "caches": ["cache-id-from-server-config"]
    }
  },
  "checks": {
    "check-name": {
      "shell": "shell-derivation-attr",
      "command": "command to run",
      "allow_network": false,
      "ro_bind": ["/path/to/readonly/bind"]
    }
  }
}
```

## Documentation

- **[GitHub App Setup Guide](docs/github-app-setup.md)** - Complete guide for creating and configuring GitHub Apps with 8 credential sources
- **[Cache Configuration Guide](docs/configure-caches.md)** - Detailed cache setup and credential management
- **[LRU Cache Operations](docs/lru-cache-operational-runbook.md)** - LRU cache tuning and monitoring
- **[Post-Build Hooks](docs/post-build-hooks.md)** - Hook system implementation details
- **[Architecture](docs/ARCHITECTURE.md)** - Detailed implementation architecture
- **[Design](DESIGN.md)** - High-level design philosophy

## Deployment

### Production Recommendations

1. **Use HashiCorp Vault or AWS Secrets Manager** for credential storage
2. **Enable cache permissions** to restrict which repositories can use which caches
3. **Configure remote builders** for parallel build capacity
4. **Set up Prometheus monitoring** to track build queue metrics
5. **Use systemd** for process management and automatic restarts
6. **Enable audit logging** for security compliance
7. **Configure LRU cache capacity** based on your repository size:
   - Small repos (<1k packages): 10,000 nodes
   - Medium repos (1k-10k packages): 100,000 nodes (default)
   - Large repos (>10k packages): 500,000+ nodes

See [docs/lru-cache-operational-runbook.md](docs/lru-cache-operational-runbook.md) for tuning guidance.

### Security Best Practices

- Never commit credentials to version control
- Use separate GitHub Apps for different environments
- Implement repository and branch restrictions via permissions
- Run eka-ci server as non-root user
- Enable systemd sandboxing features
- Rotate credentials regularly (90 days for production)
- Monitor webhook delivery for suspicious activity
- Use HTTPS with valid SSL certificates

## Monitoring

### Prometheus Metrics

eka-ci exposes metrics at `/metrics`:

- Build queue depth and throughput
- Build success/failure rates
- Graph cache hit rate and eviction stats
- Remote builder health and utilization
- Webhook processing latency

Example Prometheus query:
```promql
# Build queue depth
eka_ci_build_queue_depth

# Cache hit rate
rate(eka_ci_graph_cache_hits_total[5m]) /
  (rate(eka_ci_graph_cache_hits_total[5m]) + rate(eka_ci_graph_cache_misses_total[5m]))
```

### Logging

Structured logs with configurable levels:

```bash
# Set log level via environment
RUST_LOG=info eka-ci-server

# Filter specific modules
RUST_LOG=eka_ci_server::scheduler=debug,eka_ci_server=info eka-ci-server

# View systemd logs
journalctl -u eka-ci -f
```

## Development Roadmap

### MVP (✅ Complete)

- [x] GitHub App registration workflow
- [x] Receive PR events through webhooks
- [x] Send check runs to PRs
- [x] Git checkout and evaluation
- [x] Evaluate derivation differences between branches
- [x] Queue changed derivations for build
- [x] Remote builder support
- [x] Build log capture

### Beta (🚧 In Progress)

**Backend:**
- [x] GitHub OAuth user registration
- [x] Push successful builds to cache (framework ready)
- [x] Calculate metrics: closure size, dependencies
- [x] Dedicated FOD build queue
- [x] Multi-source credential support
- [x] Cache permission system

**Frontend:**
- [ ] GitHub OAuth integration for web UI
- [ ] PR review portal with ordered list
- [ ] Textual diff viewing
- [ ] Metrics visualization
- [ ] Approval and merge capabilities

### Future Enhancements

- [ ] "OfBorg" convention support (commit message attr paths)
- [ ] Flake checks evaluation mode
- [ ] Flake develop actions (impure commands)
- [ ] Auto-scaling remote builders
- [x] Multi-GitHub App support with automatic selection
- [ ] GCP Secret Manager and Azure Key Vault integration
- [ ] Automatic credential rotation

## Contributing

We welcome contributions! Areas that need help:

1. **Web UI Development** - Elm frontend for PR review portal
2. **Post-Build Hooks** - Complete integration with main service
3. **Webhook Signature Verification** - Implement HMAC verification
4. **Metrics Calculation** - Closure size and dependency analysis
5. **Documentation** - More examples and use cases

## Support

- **Issues**: https://github.com/ekala-project/eka-ci/issues
- **Discussions**: https://github.com/ekala-project/eka-ci/discussions

## License

[License information to be added]

## Credits

EkaCI is developed for the Ekala project by Jonathan Ringer and contributors.
