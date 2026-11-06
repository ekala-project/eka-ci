# EkaCI

Continuous Integration server purpose-built for Nix projects. EkaCI answers "should I merge this PR?" as quickly as possible by evaluating only what changed, building only what's affected, and presenting results as concise change summaries rather than walls of logs.

Full user documentation is published at <https://ekala-project.github.io/eka-ci/>.

## Why EkaCI

Curating a Nix repository shouldn't be bottlenecked by manual review. EkaCI automates the tedious parts:

- **Differential evaluation** -- only builds derivations that actually changed between base and head
- **Change summaries** -- added, removed, newly-succeeding, newly-failing, still-failing builds at a glance
- **Closure size tracking** -- per-commit output and closure sizes with threshold warnings
- **Dependency failure tracing** -- trace any failure back to its root-cause derivation
- **Release channels** -- gated branch promotion when required jobs pass
- **Search index generation** -- SQLite FTS5 databases for package/file/option search (powers ekapkgs-cli)

## Feature Comparison

|                                  | EkaCI | Hydra | ofborg |
|----------------------------------|-------|-------|--------|
| **Differential eval**            | Yes -- only changed drvs | No -- full jobset | No -- full eval |
| **Dependency-aware scheduling**  | LRU graph with transitive failure propagation | No | No |
| **Lazy check-run creation**      | Only new/changed/failed builds | All jobs | All jobs |
| **Change summaries on PRs**      | Built-in (added/removed/regressed) | Manual | Manual |
| **Release channels**             | Gated fast-forward promotion | Jobset-based channels | N/A |
| **Search index generation**      | Post-promotion SQLite FTS5 | Separate service | N/A |
| **Multi-forge support**          | GitHub, GitLab, Gitea | N/A | GitHub only |
| **Sandboxed evaluation**         | bubblewrap + landlock + pasta | Nix sandbox only | Nix sandbox only |
| **Remote builder resilience**    | Circuit-breaker with backoff probes | None | None |
| **Binary cache push**            | S3, Cachix, Attic with per-repo/branch ACLs | S3 | N/A |
| **Credential sources**           | 10 (Vault, AWS SM, systemd creds, ...) | Environment | Environment |
| **Build metrics**                | Prometheus (queue depth, phase timing, cache hit rate) | Prometheus | N/A |
| **MCP endpoint for AI agents**   | `/v1/mcp` with 6 diagnostic tools | N/A | N/A |
| **Closure size tracking**        | Output + closure size with historical baseline | N/A | N/A |
| **gRPC builder protocol**        | Dynamic registration and dispatch | N/A | N/A |

## Features

### Nix Evaluation and Building

- Differential evaluation via `nix-eval-jobs` -- only changed derivations are built
- In-memory dependency graph with configurable LRU cache (default 100k nodes)
- Three-tier scheduler: ingress (dependency eligibility) -> build queue (platform routing) -> platform queue (builder selection)
- Dedicated FOD queue to prevent network-bound fetches from starving compute builds
- Multi-platform: x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin
- `requiredSystemFeatures` respected during builder selection
- Build retry with automatic transitive failure propagation and recovery
- Configurable timeouts: output-inactivity and absolute wall-clock

### Release Channels

- Gated promotion: fast-forward a target branch only when required jobs succeed
- Job classification: `required` (must pass) vs `packages` (tracked but non-blocking)
- SHA coalescing: latest commit wins during in-flight evaluation
- Idempotent terminal decisions prevent replay on webhook duplication
- Audit trail with timestamps for every promotion decision
- Triggers search-index generation on successful promotion

### Forge Integration

- **GitHub**: webhooks, check runs, merge queue, OAuth, multi-app credential routing
- **GitLab**: webhook support with per-instance configuration
- **Gitea**: webhook support with per-instance configuration
- Lazy check-run creation: only surfaces new, changed, or failed builds to reduce noise

### Security

- **Sandboxed evaluation and checks**: bubblewrap + landlock with cleared environment
- **Filtered network**: `pasta`-based egress filtering blocks host and private network access; Nix daemon builds use the daemon's own network
- **Credential isolation**: server credentials never exposed to repository configs
- **HMAC-SHA256 webhook verification**
- **NixOS module** with strict systemd hardening (ProtectSystem=strict, PrivateTmp, etc.)

### Binary Cache Integration

- Backends: S3 (`nix copy`), Cachix, Attic
- Automatic push of successful builds to configured caches
- Per-repository and per-branch access control via glob patterns
- 10 credential sources: environment variables, file (JSON/key=value), HashiCorp Vault, AWS Secrets Manager, AWS profiles, systemd credentials (TPM2), instance metadata (EC2/GCP/Azure), GitHub App key files, Cachix tokens, and `"none"`

### Remote Builders

- SSH-based with per-builder concurrency limits and platform partitioning
- Circuit-breaker: auto-disables builders after 3 connection failures in 120s, recovers via exponential-backoff probes (30s -- 10min)
- gRPC builder protocol for dynamic self-registration and bidirectional build dispatch
- Nix daemon wire protocol (harmonia) for fast store queries with connection pooling

### Observability

- Prometheus endpoint (`/metrics`): queue depth, success/failure rates, phase timing, cache hit rates, builder health
- Build log capture with WebSocket streaming
- Structured logging via `tracing`
- Closure and output size tracking with historical baselines and configurable threshold warnings

### Search Index Generation

Generates a compressed SQLite database (`search.db.zst`) per channel after promotion:

- **Packages**: attr path, pname, version, description, outputs, main program
- **Files**: every installed file from successful builds (replaces `nix-locate`)
- **Options**: NixOS/ekaOS configuration options

Uploaded to S3 or local filesystem. Powers instant FTS5 lookups in ekapkgs-cli.

### MCP Integration

Streamable HTTP endpoint at `/v1/mcp` for AI agent diagnostics. Tools: `list_failing_prs`, `get_pr_status`, `get_failing_gates`, `get_build_log`, `get_failure_chain`, `get_drv_details`.

### CLI

The `ekaci` CLI communicates over a Unix socket:

- `ekaci pr` -- PR status and active jobsets
- `ekaci job` / `ekaci jobs` -- trigger builds, query status
- `ekaci drv info` / `ekaci drv deps` -- derivation details and dependency trees
- `ekaci log` -- stream build logs
- `ekaci check` -- run repository checks locally in the sandbox
- `ekaci channel status` -- promotion history
- `ekaci resync-checks` -- re-sync check-run states to the forge

## Quick Start

### Prerequisites

- Nix package manager
- A forge account with admin access (GitHub, GitLab, or Gitea)
- Publicly accessible server (for webhooks)

### 1. Create a Forge App

For GitHub, follow the [GitHub App setup guide](docs/github-app-setup.md).

### 2. Configure the Server

Create `~/.config/ekaci/ekaci.toml`:

```toml
[[github_apps]]
id = "main"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }

# Optional: binary cache
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

See [docs/server-configuration.md](docs/server-configuration.md) for the full configuration reference.

### 3. Configure a Repository

Add `.ekaci/config.json` to your repository:

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

### 4. Run

```bash
nix build
./result/bin/eka-ci-server
```

Or deploy with the NixOS module -- see [docs/nixos-module.md](docs/nixos-module.md).

## Documentation

- [Server Configuration](docs/server-configuration.md) -- full `ekaci.toml` reference
- [NixOS Module](docs/nixos-module.md) -- NixOS deployment with systemd hardening
- [GitHub App Setup](docs/github-app-setup.md) -- creating and configuring GitHub Apps
- [Cache Configuration](docs/configure-caches.md) -- binary cache setup and credentials
- [Architecture](docs/architecture.md) -- service design and message flow
- [Change Summaries](docs/change-summaries.md) -- operational runbook for PR summaries
- [LRU Cache Tuning](docs/lru-cache-tuning.md) -- graph cache sizing and monitoring
- [Post-Build Hooks](docs/post-build-hooks.md) -- hook system details
- [Design](DESIGN.md) -- high-level design philosophy

## Support

- **Issues**: https://github.com/ekala-project/eka-ci/issues
- **Discussions**: https://github.com/ekala-project/eka-ci/discussions

## License

[License information to be added]

## Credits

EkaCI is developed for the Ekala project by Jonathan Ringer and contributors.
