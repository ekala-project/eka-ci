# NixOS Module

The eka-ci flake provides a NixOS module at `nixosModules.daemon` that exposes the
service under `services.eka-ci`. The module uses the RFC-42 "settings" pattern: most
configuration is freeform TOML that gets serialized to `ekaci.toml`, with common fields
typed for validation and auto-generated documentation.

## Quick Start

```nix
{
  inputs.eka-ci.url = "github:ekala-project/eka-ci";

  outputs = { self, nixpkgs, eka-ci, ... }: {
    nixosConfigurations.example = nixpkgs.lib.nixosSystem {
      modules = [
        eka-ci.nixosModules.daemon
        {
          services.eka-ci = {
            enable = true;
            environmentFile = "/run/secrets/eka-ci.env";
            settings = {
              github_apps = [{
                id = "main";
                credentials.systemd-credential.name = "github-app-key";
              }];
              security.allow_insecure_webhooks = false;
            };
          };
        }
      ];
    };
  };
}
```

The service runs as a systemd `DynamicUser` by default, stores state under
`/var/lib/eka-ci`, and listens on `127.0.0.1:3030`.

## Top-Level Options

### `services.eka-ci.enable`

**Type:** `boolean`
**Default:** `false`

Enable the EkaCI server.

### `services.eka-ci.package`

**Type:** `package`
**Default:** `pkgs.eka-ci`

Package providing the `eka_ci_server` binary.

### `services.eka-ci.user` / `services.eka-ci.group`

**Type:** `string`
**Default:** `"eka-ci"`

User and group the service runs as when `dynamicUser = false`. Ignored when
`dynamicUser = true`.

### `services.eka-ci.dynamicUser`

**Type:** `boolean`
**Default:** `true`

Use systemd's `DynamicUser=` to run the service under an ephemeral user/group. Recommended
unless you need a stable UID for filesystem permissions on shared storage.

### `services.eka-ci.openFirewall`

**Type:** `boolean`
**Default:** `false`

Open `settings.web.port` in the system firewall.

### `services.eka-ci.environmentFile`

**Type:** `null or path`
**Default:** `null`

Path to a file passed to systemd as `EnvironmentFile=`. Use this to provide secrets such
as `GITHUB_WEBHOOK_SECRET`, `GITHUB_OAUTH_CLIENT_SECRET`, `JWT_SECRET`, `VAULT_TOKEN`,
AWS keys, and any environment variables referenced from
`settings.caches.*.credentials.env.vars`.

The file is read by systemd at start time and never enters the Nix store.

**Example (`/run/secrets/eka-ci.env`):**
```bash
GITHUB_WEBHOOK_SECRET=whsec_...
GITHUB_OAUTH_CLIENT_SECRET=...
JWT_SECRET=...
VAULT_TOKEN=s.abc123...
AWS_ACCESS_KEY_ID=AKIA...
AWS_SECRET_ACCESS_KEY=...
```

### `services.eka-ci.credentials`

**Type:** `attribute set of path`
**Default:** `{}`

Map of credential name to file path, wired through systemd's `LoadCredential=`. Each entry
becomes available inside the unit at `$CREDENTIALS_DIRECTORY/<name>` and can be referenced
from `ekaci.toml` via the `systemd-credential` credential source.

**Example:**
```nix
services.eka-ci = {
  credentials.github-app-key = "/run/secrets/github-app.json";
  settings.github_apps = [{
    id = "main";
    credentials.systemd-credential.name = "github-app-key";
  }];
};
```

Pairs naturally with [sops-nix](https://github.com/Mic92/sops-nix),
[agenix](https://github.com/ryantm/agenix), or
[systemd-creds](https://www.freedesktop.org/software/systemd/man/systemd-creds.html).

### `services.eka-ci.extraEnvironment`

**Type:** `attribute set of string`
**Default:** `{}`

Additional `Environment=` entries passed to the systemd unit.

**Example:**
```nix
services.eka-ci.extraEnvironment = {
  RUST_LOG = "eka_ci_server::scheduler=debug,info";
};
```

## Settings Options

The `services.eka-ci.settings` submodule is freeform: any key not explicitly listed below is
still accepted and serialized to `ekaci.toml` as-is. Typed options provide validation and
documentation for the most common fields.

### `settings.db_path`

**Type:** `null or path`
**Default:** `null`

SQLite database path. When `null` the server falls back to `$XDG_DATA_HOME/ekaci/sqlite.db`,
which under this module resolves to `/var/lib/eka-ci/ekaci/sqlite.db`.

### `settings.logs_dir`

**Type:** `null or path`
**Default:** `null`

Directory where build logs are stored. When `null` the server falls back to
`$XDG_DATA_HOME/ekaci/build-logs`.

### `settings.require_approval`

**Type:** `boolean`
**Default:** `false`

Require maintainer approval before building PRs from external contributors.

### `settings.merge_queue_require_approval`

**Type:** `boolean`
**Default:** `false`

Require approval before building entries pulled from the GitHub merge queue.

### `settings.build_no_output_timeout_seconds`

**Type:** `integer between 30 and 86400`
**Default:** `1200`

Number of seconds with no build output after which a build is considered hung.

### `settings.build_max_duration_seconds`

**Type:** `integer between 60 and 604800`
**Default:** `14400`

Hard upper bound, in seconds, on total build wall-clock time.

### `settings.graph_lru_capacity`

**Type:** `positive integer`
**Default:** `100000`

Capacity of the in-memory derivation-graph LRU cache, in nodes. See
[LRU Cache Tuning](./lru-cache-tuning.md) for sizing guidance.

### `settings.default_merge_method`

**Type:** `one of "merge", "squash", "rebase"`
**Default:** `"squash"`

Default merge method used by the `@eka-ci merge` PR comment command.

### `settings.web`

**Type:** `submodule`

HTTP server settings.

- **`web.address`** (`string`, default `"127.0.0.1"`): IPv4 address the HTTP server binds to.
- **`web.port`** (`port`, default `3030`): TCP port the HTTP server binds to.
- **`web.bundle_path`** (`null or path`, default `null`): Optional path to a pre-built web UI bundle.
- **`web.allowed_origins`** (`list of string`, default `[]`): CORS allow-list. Each entry must
  be a fully-qualified `http://` or `https://` origin with no path, query, fragment, or `*`
  wildcard. An empty list rejects all cross-origin requests.

### `settings.unix`

**Type:** `submodule`

Unix domain socket settings used by the CLI client.

- **`unix.socket_path`** (`null or path`, default `null`): Unix domain socket the CLI client
  connects to. When `null` the server falls back to `$XDG_RUNTIME_DIR/ekaci.socket`, which
  under this module resolves to `/run/eka-ci/ekaci.socket`.

### `settings.oauth`

**Type:** `submodule`

OAuth settings for the (optional) web UI.

- **`oauth.client_id`** (`null or string`, default `null`): GitHub OAuth client ID. May also
  be supplied via the `GITHUB_OAUTH_CLIENT_ID` environment variable (preferred — see
  `environmentFile`).
- **`oauth.client_secret`** (`null or string`, default `null`): GitHub OAuth client secret.
  **Avoid setting this in Nix** — values here end up in the world-readable Nix store. Use
  `environmentFile` to supply `GITHUB_OAUTH_CLIENT_SECRET` instead.
- **`oauth.redirect_url`** (`null or string`, default `null`): OAuth callback URL. Defaults
  to `http://{web.address}:{web.port}/github/auth/callback` when unset.
- **`oauth.jwt_secret`** (`null or string`, default `null`): JWT signing secret. **Avoid
  setting this in Nix.** Provide `JWT_SECRET` via `environmentFile`. When omitted entirely,
  the server generates an ephemeral 256-bit secret on each start (sessions invalidate across
  restarts).

### `settings.security`

**Type:** `submodule`

Security-related settings.

- **`security.max_hook_timeout_seconds`** (`integer between 1 and 86400`, default `300`):
  Maximum wall-clock time, in seconds, that any post-build hook is allowed to run.
- **`security.audit_hooks`** (`boolean`, default `true`): Emit structured audit log records
  every time a hook runs.
- **`security.webhook_secret`** (`null or string`, default `null`): GitHub webhook HMAC
  secret. **Avoid setting this in Nix.** Provide `GITHUB_WEBHOOK_SECRET` via
  `environmentFile`. The server refuses to start if no webhook secret is available unless
  `allow_insecure_webhooks` is `true`.
- **`security.allow_insecure_webhooks`** (`boolean`, default `false`): Allow the server to
  start without a webhook secret. Intended for local development only; never enable in
  production.
- **`security.allow_private_cache_hosts`** (`boolean`, default `false`): Allow cache
  destinations whose DNS resolves to private/loopback addresses. Disables built-in SSRF
  protection; only enable in trusted, isolated networks.

### `settings.caches`

**Type:** `list of submodule`
**Default:** `[]`

List of binary caches the server may push to.

Each cache entry has the following fields:

- **`id`** (`string`, **required**): Cache identifier referenced from `.eka-ci/config.json`.
- **`cache_type`** (`one of "nix-copy", "cachix", "attic"`, **required**): Backend type for
  this cache.
- **`destination`** (`string`, **required**): Destination URL passed to the chosen backend.
  Validated for SSRF unless `settings.security.allow_private_cache_hosts` is set.
- **`credentials`** (freeform, **required**): Credential source. See
  [Credential Sources](#credential-sources) below.
- **`permissions`** (`submodule`, default allows all): Repository/branch access control.
  - **`allow_all`** (`boolean`, default `true`): When `true`, ignores `allowed_repos` and
    `allowed_branches` and grants access to every repository and branch.
  - **`allowed_repos`** (`list of string`, default `[]`): Glob patterns of `owner/repo`
    strings that are permitted to use this entry.
  - **`allowed_branches`** (`list of string`, default `[]`): Glob patterns of branch names
    permitted to use this entry.

**Example:**
```nix
settings.caches = [{
  id = "production-s3";
  cache_type = "nix-copy";
  destination = "s3://my-bucket/nix-cache?region=us-east-1";
  credentials.env.vars = [ "AWS_ACCESS_KEY_ID" "AWS_SECRET_ACCESS_KEY" ];
  permissions = {
    allow_all = false;
    allowed_repos = [ "myorg/*" ];
    allowed_branches = [ "main" "release/*" ];
  };
}];
```

### `settings.github_apps`

**Type:** `list of submodule`
**Default:** `[]`

List of GitHub Apps the server authenticates as.

Each GitHub App entry has the following fields:

- **`id`** (`string`, **required**): GitHub App identifier.
- **`credentials`** (freeform, **required**): Credential source. See
  [Credential Sources](#credential-sources) below.
- **`permissions`** (`submodule`, default allows all): Same structure as
  `settings.caches.*.permissions`.

**Example:**
```nix
settings.github_apps = [{
  id = "main";
  credentials.file.path = "/run/secrets/github-app.json";
  permissions = {
    allow_all = false;
    allowed_repos = [ "myorg/*" ];
  };
}];
```

## Credential Sources

Both `settings.caches.*.credentials` and `settings.github_apps.*.credentials` accept one of
ten credential source variants. The field is freeform (not exhaustively typed) so all variants
serialize correctly. Choose the one that matches your secret-management setup:

### 1. Environment variables

```nix
credentials.env.vars = [ "AWS_ACCESS_KEY_ID" "AWS_SECRET_ACCESS_KEY" ];
```

The server reads the listed environment variables at runtime. Provide them via
`environmentFile`.

### 2. File

```nix
credentials.file.path = "/etc/eka-ci/creds.json";
```

The server reads a JSON or `KEY=VALUE` file at the given path.

### 3. AWS profile

```nix
credentials.aws-profile.profile = "production";
```

The server reads credentials from `~/.aws/credentials` using the named profile.

### 4. Cachix token

```nix
credentials.cachix-token.env_var = "CACHIX_AUTH_TOKEN";
```

The server reads a Cachix auth token from the named environment variable.

### 5. HashiCorp Vault

```nix
credentials.vault = {
  address = "https://vault.example.com:8200";
  secret_path = "secret/data/eka-ci/s3-cache";
  token_env = "VAULT_TOKEN";  # optional, defaults to "VAULT_TOKEN"
  namespace = "production";    # optional
};
```

The server authenticates to Vault using the token from `token_env` and reads the secret at
`secret_path`.

### 6. AWS Secrets Manager

```nix
credentials.aws-secrets-manager = {
  secret_name = "eka-ci/s3-credentials";
  region = "us-east-1";  # optional, falls back to AWS_REGION env var
};
```

The server uses AWS SDK credential resolution (environment, instance metadata, profiles) to
authenticate to AWS Secrets Manager and reads the named secret.

### 7. systemd credential

```nix
credentials.systemd-credential.name = "github-app-key";
```

The server reads the credential from `$CREDENTIALS_DIRECTORY/<name>`. Pair this with the
top-level `services.eka-ci.credentials` option:

```nix
services.eka-ci.credentials.github-app-key = "/run/secrets/github-app.json";
```

### 8. Instance metadata

```nix
credentials = "instance-metadata";
```

The server retrieves credentials from EC2/GCP/Azure instance metadata. No configuration needed.

### 9. GitHub App key file

```nix
credentials.github-app-key-file = {
  app_id_env = "GITHUB_APP_ID";
  key_file = "/etc/eka-ci/github-app.pem";
};
```

The server reads the GitHub App ID from the named environment variable and the PEM-encoded
private key from the file.

### 10. None

```nix
credentials = "none";
```

No authentication. Only valid for public caches.

## Systemd Hardening

The module applies aggressive systemd hardening by default:

- `DynamicUser = true` (ephemeral user/group)
- `ProtectSystem = "strict"` (read-only `/usr`, `/boot`, `/efi`)
- `ProtectHome = true` (no access to `/home`, `/root`)
- `PrivateTmp = true` (isolated `/tmp`)
- `PrivateDevices = true` (empty `/dev`)
- `NoNewPrivileges = true` (no privilege escalation)
- `ProtectKernelModules/Tunables/Logs = true`
- `ProtectControlGroups/Clock/Hostname = true`
- `RestrictNamespaces/Realtime/SUIDSGID = true`
- `RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ]`
- `LockPersonality = true`
- `MemoryDenyWriteExecute = true`
- `SystemCallArchitectures = "native"`
- `SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ]`
- Empty `CapabilityBoundingSet` and `AmbientCapabilities`
- `UMask = "0077"`

If you need to relax any of these, override `systemd.services.eka-ci.serviceConfig` in your
configuration.

## Complete Example

```nix
{ config, ... }:
{
  services.eka-ci = {
    enable = true;
    openFirewall = false;  # Behind a reverse proxy

    environmentFile = config.sops.secrets.eka-ci-env.path;

    credentials = {
      github-app-key = config.sops.secrets.github-app-json.path;
      s3-creds       = config.sops.secrets.s3-json.path;
    };

    extraEnvironment.RUST_LOG = "info";

    settings = {
      web = {
        address = "127.0.0.1";
        port = 3030;
        allowed_origins = [ "https://ci.example.com" ];
      };

      graph_lru_capacity = 200000;  # Large repo
      default_merge_method = "squash";

      security = {
        audit_hooks = true;
        allow_insecure_webhooks = false;
      };

      github_apps = [{
        id = "main";
        credentials.systemd-credential.name = "github-app-key";
        permissions = {
          allow_all = false;
          allowed_repos = [ "myorg/*" ];
        };
      }];

      caches = [
        {
          id = "s3-production";
          cache_type = "nix-copy";
          destination = "s3://my-bucket/nix-cache?region=us-east-1";
          credentials.systemd-credential.name = "s3-creds";
          permissions = {
            allow_all = false;
            allowed_repos = [ "myorg/production-*" ];
            allowed_branches = [ "main" ];
          };
        }
        {
          id = "cachix-public";
          cache_type = "cachix";
          destination = "myorg";
          credentials.cachix-token.env_var = "CACHIX_AUTH_TOKEN";
        }
      ];
    };
  };

  # Reverse proxy
  services.nginx.virtualHosts."ci.example.com" = {
    enableACME = true;
    forceSSL = true;
    locations."/" = {
      proxyPass = "http://127.0.0.1:3030";
      proxyWebsockets = true;
    };
  };
}
```

## See Also

- [Module Reference](./nixos-module-reference.md) — auto-generated complete option reference
- [Server Configuration](./server-configuration.md) — detailed `ekaci.toml` reference
- [Configuring Caches](./configure-caches.md) — cache setup and credential management
- [GitHub App Setup](./github-app-setup.md) — creating and configuring GitHub Apps
- [LRU Cache Tuning](./lru-cache-tuning.md) — sizing `graph_lru_capacity`
