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

# Evaluation sandbox
[eval]
timeout_secs    = 1800    # kill nix-eval-jobs after 30 minutes
memory_limit_mb = 8192    # RLIMIT_AS per sandboxed process
allowed_uris    = ["https://github.com/NixOS/nixpkgs/"]

# Network policy for every sandbox with network access
[sandbox]
network_allow = ["10.20.0.5/32"]   # e.g. an internal binary cache
network_deny  = []

# Repository checks (.ekaci/config.json "checks")
[checks]
timeout_secs    = 1800
memory_limit_mb = 8192

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

### `[eval]`

Every `nix-eval-jobs` run (PR/branch evaluation, `passthru.tests`, and re-evaluation of
garbage-collected `.drv` files) executes inside an evaluation sandbox, because the Nix code
comes from untrusted pull requests. One evaluation is not covered yet: listing a flake's
`checks`/`packages` (`nix eval` of the pull request's flake, used when those flake outputs
are enabled) still runs directly on the server, with its network and environment and no
limits; it is slated for removal in a later change.

- **bubblewrap** (`bwrap --unshare-all --clearenv --die-with-parent --new-session`): the
  process sees no inherited environment variables (only `PATH`,
  `HOME`, `TMPDIR`, `NIX_REMOTE=daemon`), a private `/tmp` and home, and of the host
  filesystem only `/nix/store`, the Nix daemon socket, the repository worktree (read-only)
  and a few files from `/etc` (`ssl/certs`, `resolv.conf`, `hosts`, `passwd`,
  `group`, `static`). Server secrets, `$CREDENTIALS_DIRECTORY` and the SQLite database are
  not reachable.
- **landlock**, applied by `ekaci-sandbox-helper` inside the namespace, restricts file
  access to the same paths. It is best-effort: on a kernel without landlock the sandbox
  degrades to bubblewrap only and the server logs a warning at startup.
- **Nix** runs with `restrict-eval = true` and an explicit `allowed-uris`, so
  `builtins.readFile`/`import` outside the worktree and eval-time fetches to unlisted
  URLs fail.
- **Network** is filtered (see [`[sandbox]`](#sandbox)): even an allowlisted URI cannot
  reach the host or its private networks directly.

| Key | Default | Meaning |
| --- | --- | --- |
| `timeout_secs` | `1800` | Wall-clock limit per evaluation (10–86400). On expiry the whole sandbox is killed and the evaluation fails with a timeout error. |
| `memory_limit_mb` | `8192` | `RLIMIT_AS` for each process in the sandbox (≥ 1024). `nix-eval-jobs` workers are recycled at half this value. |
| `allowed_uris` | `[]` | URI prefixes eval-time fetchers may access (Nix `allowed-uris`). |

**`allowed_uris` and pinned nixpkgs.** With the default empty list, *any* eval-time fetch
fails. Repositories that pin inputs with `builtins.fetchTarball`/`fetchurl`/`fetchGit`
(e.g. `fetchTarball "https://github.com/NixOS/nixpkgs/archive/<rev>.tar.gz"`) need the URL
prefix listed. Fixed-output derivations such as `pkgs.fetchurl` are not affected: they
run at build time, not eval time. Note also that `NIX_PATH` is not passed through, so
`<nixpkgs>` lookups do not resolve inside evaluations.

**Requirements.** `bwrap` and `pasta` (from [passt](https://passt.top)) must be on the
server's `PATH`, and the host must allow unprivileged user namespaces. Under systemd the
unit must not set `RestrictNamespaces=true`, its `SystemCallFilter` must permit `@mount`
and `capset`, `/dev/net/tun` must be accessible and `AF_NETLINK` sockets allowed (the
NixOS module does all of this). The server runs a preflight of both the isolation and the
network filter at startup and **refuses to start** if either fails — there is no
unsandboxed or unfiltered fallback.

### `[sandbox]`

Network policy shared by every sandbox that has network access: evaluation, dev-shell
capture for checks (`nix develop` / `nix-shell`), and checks with `allow_network = true`.
Checks with `allow_network = false` get no direct network (`bwrap --unshare-net`); the Nix
daemon socket still works (see the daemon caveat below).

Network access goes through `pasta`, which gives the sandbox its own network namespace
and relays its traffic through ordinary sockets of the server. Inside that namespace
`unreachable` routes cover:

- private and special ranges: `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10` (CGNAT),
  `169.254.0.0/16` (link-local, including cloud metadata at `169.254.169.254`),
  `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.168.0.0/16`, `198.18.0.0/15`,
  `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`;
- every IPv4 address of the server itself (snapshotted at each spawn), so services the
  server exposes on a public address are unreachable too. Only addresses on the server's
  interfaces are covered: behind 1:1 NAT (a cloud elastic or floating IP) the public
  address is not on an interface and stays reachable; add it to `network_deny`;
- the server's loopback: `pasta` runs with `--no-map-gw` and without loopback port
  forwarding, so `127.0.0.1` inside the sandbox is the sandbox's own loopback.

IPv6 is disabled in the sandbox. DNS works through a fixed resolver address that `pasta`
forwards to the server's resolver. The routes are installed before the sandboxed code
starts, from a user namespace it cannot act in: code in the sandbox gets `EPERM` when it
tries to change them.

With this filter, `allow_network` in a repository's `.ekaci/config.json` gives a pull
request internet egress only, never the server's private networks or local services
*directly*. It does not close the channel below, which exists with or without it.

**The Nix daemon is outside the filter.** Every sandbox reaches the Nix daemon socket (Nix
needs it to evaluate and build). Code from a pull request, including a check with
`allow_network = false` and an evaluation via import-from-derivation, can therefore ask the
daemon to build a fixed-output derivation, which Nix runs with the daemon's network: it can
reach cloud metadata, private ranges and host services, and its output or error message can
come back in the check log. This is accepted for now. To close it, filter the egress of the
Nix build users on the host (for example an nftables rule on `meta skgid nixbld` dropping
the same ranges), keep the daemon's build sandbox on (the default), and never list the
server's user in `trusted-users`.

| Key | Default | Meaning |
| --- | --- | --- |
| `network_allow` | `[]` | IPv4 prefixes (`a.b.c.d/n`, or a bare address for `/32`) reachable despite the list above, e.g. an internal binary cache. |
| `network_deny` | `[]` | Additional IPv4 prefixes to make unreachable. |
| `helper_path` | next to the server binary, else `PATH` | Location of `ekaci-sandbox-helper`, used by every sandbox. |

The longest matching prefix wins; on an exact tie the `network_allow` entry wins. This
section is server-only: repositories cannot change it.

### `[checks]`

Limits for check commands from `.ekaci/config.json`. A check runs in the same sandbox as
evaluation (bubblewrap + landlock), with the checkout read-write, its `.git` read-only and
`/nix/store` read-only, and exactly the environment of its dev shell — nothing from the
server. Capturing that environment (`nix develop` / `nix-shell`) evaluates untrusted Nix,
so it runs in the sandbox as well, with a filtered network and the `[eval]` limits.

| Key | Default | Meaning |
| --- | --- | --- |
| `timeout_secs` | `1800` | Wall-clock limit per check command (10–86400). The whole sandbox is killed on expiry and the check fails. |
| `memory_limit_mb` | `8192` | `RLIMIT_AS` for each process of a check (≥ 1024). |

Both limits apply per process (`RLIMIT_AS`); a check that forks many processes can use more
memory in total. Run the server under a systemd `MemoryMax=` to cap the whole service.

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
