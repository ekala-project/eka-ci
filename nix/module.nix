flake:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkOption
    mkIf
    mkMerge
    types
    optional
    optionalAttrs
    mapAttrsToList
    ;

  cfg = config.services.eka-ci;
  settingsFormat = pkgs.formats.toml { };

  # Permission submodule reused for both caches and github_apps.
  permissionsType = types.submodule {
    options = {
      allow_all = mkOption {
        type = types.bool;
        default = true;
        description = ''
          When `true`, ignores {option}`allowed_repos` and
          {option}`allowed_branches` and grants access to every repository
          and branch.
        '';
      };
      allowed_repos = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "myorg/*" ];
        description = ''
          Glob patterns of `owner/repo` strings that are permitted to use this
          entry. Ignored when {option}`allow_all` is `true`.
        '';
      };
      allowed_branches = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "main" "release/*" ];
        description = ''
          Glob patterns of branch names permitted to use this entry. Ignored
          when {option}`allow_all` is `true`.
        '';
      };
    };
  };

  # Per-cache submodule. Credentials are kept freeform so all 10 credential
  # source variants serialise correctly without an exhaustive Nix encoding.
  cacheType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      id = mkOption {
        type = types.str;
        example = "production-s3";
        description = "Cache identifier referenced from `.eka-ci/config.json`.";
      };
      cache_type = mkOption {
        type = types.enum [ "nix-copy" "cachix" "attic" ];
        example = "nix-copy";
        description = "Backend type for this cache.";
      };
      destination = mkOption {
        type = types.str;
        example = "s3://my-bucket/nix-cache?region=us-east-1";
        description = ''
          Destination URL passed to the chosen backend. Validated for SSRF
          unless {option}`settings.security.allow_private_cache_hosts` is set.
        '';
      };
      credentials = mkOption {
        type = settingsFormat.type;
        example = {
          env = { vars = [ "AWS_ACCESS_KEY_ID" "AWS_SECRET_ACCESS_KEY" ]; };
        };
        description = ''
          Credential source. One of:

          - `{ env = { vars = [ ... ]; }; }`
          - `{ file = { path = "/etc/..."; }; }`
          - `{ aws-profile = { profile = "..."; }; }`
          - `{ cachix-token = { env_var = "..."; }; }`
          - `{ vault = { address; secret_path; token_env ? "VAULT_TOKEN"; namespace ? null; }; }`
          - `{ aws-secrets-manager = { secret_name; region ? null; }; }`
          - `{ systemd-credential = { name = "..."; }; }`
          - `"instance-metadata"`
          - `{ github-app-key-file = { app_id_env; key_file; }; }`
          - `"none"`

          Prefer `systemd-credential` paired with the top-level
          {option}`services.eka-ci.credentials` option to keep secrets out of
          the world-readable Nix store.
        '';
      };
      permissions = mkOption {
        type = permissionsType;
        default = { };
        description = "Repository/branch access control for this cache.";
      };
    };
  };

  githubAppType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      id = mkOption {
        type = types.str;
        example = "main";
        description = "GitHub App identifier referenced from per-app permission lookups.";
      };
      credentials = mkOption {
        type = settingsFormat.type;
        example = { file = { path = "/etc/eka-ci/github-app.json"; }; };
        description = ''
          Credential source. Same shape as
          {option}`services.eka-ci.settings.caches.*.credentials`.
        '';
      };
      permissions = mkOption {
        type = permissionsType;
        default = { };
        description = "Repository/branch access control for this GitHub App.";
      };
    };
  };

  webType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      address = mkOption {
        type = types.str;
        default = "127.0.0.1";
        description = "IPv4 address the HTTP server binds to.";
      };
      port = mkOption {
        type = types.port;
        default = 3030;
        description = "TCP port the HTTP server binds to.";
      };
      bundle_path = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Optional path to a pre-built web UI bundle.";
      };
      allowed_origins = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "https://app.example.com" ];
        description = ''
          CORS allow-list. Each entry must be a fully-qualified `http://` or
          `https://` origin with no path, query, fragment, or `*` wildcard.
          An empty list rejects all cross-origin requests.
        '';
      };
    };
  };

  unixType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      socket_path = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          Unix domain socket the CLI client connects to. When `null` the
          server falls back to `$XDG_RUNTIME_DIR/ekaci.socket`, which under
          this module resolves to `/run/eka-ci/ekaci.socket`.
        '';
      };
    };
  };

  oauthType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      client_id = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          GitHub OAuth client ID. May also be supplied via the
          `GITHUB_OAUTH_CLIENT_ID` environment variable (preferred — see
          {option}`services.eka-ci.environmentFile`).
        '';
      };
      client_secret = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          GitHub OAuth client secret. **Avoid setting this in Nix** — values
          here end up in the world-readable Nix store. Use
          {option}`services.eka-ci.environmentFile` to supply
          `GITHUB_OAUTH_CLIENT_SECRET` instead.
        '';
      };
      redirect_url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          OAuth callback URL. Defaults to
          `http://{web.address}:{web.port}/github/auth/callback` when unset.
        '';
      };
      jwt_secret = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          JWT signing secret. **Avoid setting this in Nix.** Provide
          `JWT_SECRET` via {option}`services.eka-ci.environmentFile`. When
          omitted entirely, the server generates an ephemeral 256-bit secret
          on each start (sessions invalidate across restarts).
        '';
      };
    };
  };

  securityType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      max_hook_timeout_seconds = mkOption {
        type = types.ints.between 1 86400;
        default = 300;
        description = "Maximum wall-clock time, in seconds, that any post-build hook is allowed to run.";
      };
      audit_hooks = mkOption {
        type = types.bool;
        default = true;
        description = "Emit structured audit log records every time a hook runs.";
      };
      webhook_secret = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          GitHub webhook HMAC secret. **Avoid setting this in Nix.** Provide
          `GITHUB_WEBHOOK_SECRET` via
          {option}`services.eka-ci.environmentFile`.

          The server refuses to start if no webhook secret is available
          unless {option}`allow_insecure_webhooks` is `true`.
        '';
      };
      allow_insecure_webhooks = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Allow the server to start without a webhook secret. Intended for
          local development only; never enable in production.
        '';
      };
      allow_private_cache_hosts = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Allow cache destinations whose DNS resolves to private/loopback
          addresses. Disables built-in SSRF protection; only enable in
          trusted, isolated networks.
        '';
      };
    };
  };

  settingsType = types.submodule {
    freeformType = settingsFormat.type;
    options = {
      web = mkOption {
        type = webType;
        default = { };
        description = "HTTP server settings.";
      };
      unix = mkOption {
        type = unixType;
        default = { };
        description = "Unix-domain-socket settings used by the CLI client.";
      };
      oauth = mkOption {
        type = oauthType;
        default = { };
        description = "OAuth settings for the (optional) web UI.";
      };
      security = mkOption {
        type = securityType;
        default = { };
        description = "Security-related settings.";
      };

      db_path = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          SQLite database path. When `null` the server falls back to
          `$XDG_DATA_HOME/ekaci/sqlite.db` which, under this module, resolves
          to `/var/lib/eka-ci/ekaci/sqlite.db`.
        '';
      };
      logs_dir = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          Directory where build logs are stored. When `null` the server
          falls back to `$XDG_DATA_HOME/ekaci/build-logs`.
        '';
      };
      require_approval = mkOption {
        type = types.bool;
        default = false;
        description = "Require maintainer approval before building PRs from external contributors.";
      };
      merge_queue_require_approval = mkOption {
        type = types.bool;
        default = false;
        description = "Require approval before building entries pulled from the GitHub merge queue.";
      };
      build_no_output_timeout_seconds = mkOption {
        type = types.ints.between 30 86400;
        default = 1200;
        description = "Number of seconds with no build output after which a build is considered hung.";
      };
      build_max_duration_seconds = mkOption {
        type = types.ints.between 60 604800;
        default = 14400;
        description = "Hard upper bound, in seconds, on total build wall-clock time.";
      };
      graph_lru_capacity = mkOption {
        type = types.ints.positive;
        default = 100000;
        description = ''
          Capacity of the in-memory derivation-graph LRU cache, in nodes.
          See `docs/lru-cache-tuning.md` for sizing guidance.
        '';
      };
      default_merge_method = mkOption {
        type = types.enum [ "merge" "squash" "rebase" ];
        default = "squash";
        description = "Default merge method used by the `@eka-ci merge` PR comment command.";
      };
      caches = mkOption {
        type = types.listOf cacheType;
        default = [ ];
        description = "List of binary caches the server may push to.";
      };
      github_apps = mkOption {
        type = types.listOf githubAppType;
        default = [ ];
        description = "List of GitHub Apps the server authenticates as.";
      };
    };
  };
in
{
  options.services.eka-ci = {
    enable = mkEnableOption "EkaCI, a Nix-aware Continuous Integration server";

    package = mkPackageOption pkgs "eka-ci" { };

    user = mkOption {
      type = types.str;
      default = "eka-ci";
      description = ''
        User the service runs as when {option}`dynamicUser` is `false`.
        Ignored when `dynamicUser = true`.
      '';
    };

    group = mkOption {
      type = types.str;
      default = "eka-ci";
      description = ''
        Group the service runs as when {option}`dynamicUser` is `false`.
        Ignored when `dynamicUser = true`.
      '';
    };

    dynamicUser = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Use systemd's `DynamicUser=` to run the service under an ephemeral
        user/group. Recommended unless you need a stable UID for filesystem
        permissions on shared storage.
      '';
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open {option}`settings.web.port` in the system firewall.";
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/secrets/eka-ci.env";
      description = ''
        Path to a file passed to systemd as `EnvironmentFile=`. Use this to
        provide secrets such as `GITHUB_WEBHOOK_SECRET`,
        `GITHUB_OAUTH_CLIENT_SECRET`, `JWT_SECRET`, `VAULT_TOKEN`, AWS keys,
        and any environment variables referenced from
        {option}`settings.caches.*.credentials.env.vars`. The file is read
        by systemd at start time and never enters the Nix store.
      '';
    };

    credentials = mkOption {
      type = types.attrsOf types.path;
      default = { };
      example = lib.literalExpression ''
        {
          github-app-key = "/run/secrets/github-app.json";
          s3-creds       = "/run/secrets/s3.json";
        }
      '';
      description = ''
        Map of credential name to file path, wired through systemd's
        `LoadCredential=`. Each entry becomes available inside the unit at
        `$CREDENTIALS_DIRECTORY/<name>` and can be referenced from
        `ekaci.toml` via the `systemd-credential` credential source, e.g.

        ```nix
        services.eka-ci.settings.github_apps = [
          {
            id = "main";
            credentials.systemd-credential.name = "github-app-key";
          }
        ];
        ```
      '';
    };

    extraEnvironment = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = { RUST_LOG = "eka_ci_server=debug,info"; };
      description = "Additional `Environment=` entries passed to the systemd unit.";
    };

    settings = mkOption {
      type = settingsType;
      default = { };
      description = ''
        Configuration for EkaCI, serialised verbatim to `ekaci.toml`. The
        submodule is freeform: any key not explicitly modelled here is still
        accepted and forwarded as-is to the TOML output.
      '';
    };
  };

  config = mkIf cfg.enable {
    # Make the eka-ci package available via the overlay supplied by this flake.
    nixpkgs.overlays = [ flake.overlays.default ];

    networking.firewall.allowedTCPPorts =
      lib.optional cfg.openFirewall cfg.settings.web.port;

    users.users = optionalAttrs (!cfg.dynamicUser) {
      ${cfg.user} = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/eka-ci";
      };
    };

    users.groups = optionalAttrs (!cfg.dynamicUser) {
      ${cfg.group} = { };
    };

    warnings =
      optional
        (cfg.settings.security.webhook_secret == null
          && !cfg.settings.security.allow_insecure_webhooks
          && cfg.environmentFile == null)
        ''
          services.eka-ci: no webhook secret is configured. Set one of:
            - services.eka-ci.settings.security.webhook_secret (not recommended; ends up in the Nix store)
            - services.eka-ci.environmentFile (file containing GITHUB_WEBHOOK_SECRET=...)
            - services.eka-ci.settings.security.allow_insecure_webhooks = true (development only)
          The server will refuse to start without one of these.
        '';

    systemd.services.eka-ci = {
      description = "EkaCI server";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = mkMerge [
        (let
          conf = settingsFormat.generate "ekaci.toml" cfg.settings;
        in {
          ExecStart = "${cfg.package}/bin/eka_ci_server --config-file ${conf}";

          Restart = "on-failure";
          RestartSec = 5;

          WorkingDirectory = "%S/%p";
          StateDirectory = "%p";
          RuntimeDirectory = "%p";
          RuntimeDirectoryMode = "0700";
          LogsDirectory = "%p";

          Environment = [
            "XDG_RUNTIME_DIR=/run/%p"
            "XDG_DATA_HOME=/var/lib/%p"
          ] ++ mapAttrsToList (k: v: "${k}=${v}") cfg.extraEnvironment;
        })

        (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = cfg.environmentFile;
        })

        (mkIf (cfg.credentials != { }) {
          LoadCredential = mapAttrsToList (n: p: "${n}:${p}") cfg.credentials;
        })

        (if cfg.dynamicUser then {
          DynamicUser = true;
        } else {
          User = cfg.user;
          Group = cfg.group;
        })

        # Hardening
        {
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          PrivateDevices = true;
          DevicePolicy = "closed";
          ProtectControlGroups = true;
          ProtectKernelModules = true;
          ProtectKernelTunables = true;
          ProtectKernelLogs = true;
          ProtectClock = true;
          ProtectHostname = true;
          ProtectProc = "invisible";
          ProcSubset = "pid";
          RestrictNamespaces = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ];
          LockPersonality = true;
          MemoryDenyWriteExecute = true;
          SystemCallArchitectures = "native";
          SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ];
          CapabilityBoundingSet = [ ];
          AmbientCapabilities = [ ];
          UMask = "0077";
        }
      ];
    };
  };
}
