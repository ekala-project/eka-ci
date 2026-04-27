# Installation

Eka CI is distributed as a Nix flake. The build produces two binaries:

- `eka-ci-server` — the CI server daemon.
- `ekaci` — a CLI client that talks to the server over a Unix socket.

## Build from source

```bash
git clone https://github.com/ekala-project/eka-ci.git
cd eka-ci
nix build
./result/bin/eka-ci-server --help
```

The flake exposes the standard `packages.default` attribute, so it can also be consumed from
another flake:

```nix
{
  inputs.eka-ci.url = "github:ekala-project/eka-ci";

  outputs = { self, nixpkgs, eka-ci, ... }: {
    # ...
    nixosConfigurations.example = nixpkgs.lib.nixosSystem {
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [ eka-ci.packages.${pkgs.system}.default ];
        })
      ];
    };
  };
}
```

## Run as a systemd service

A minimal unit file:

```ini
[Unit]
Description=eka-ci server
After=network.target

[Service]
Type=simple
ExecStart=/path/to/eka-ci-server
Restart=on-failure
User=eka-ci
Environment="RUST_LOG=info"
# Example: provide credentials for cache backends
Environment="VAULT_TOKEN=s.your-token"

[Install]
WantedBy=multi-user.target
```

For production deployments you will likely want to add systemd hardening
(`ProtectSystem=strict`, `PrivateTmp=true`, `NoNewPrivileges=true`, etc.) and to load
secrets via `LoadCredential=` and the `systemd` credential source documented in
[GitHub App Setup](./github-app-setup.md).

## Required state directories

By default the server stores state under paths that can be overridden in
`ekaci.toml`:

| Purpose | Default | Setting |
|---|---|---|
| SQLite database | `~/.local/share/ekaci/sqlite.db` | `db_path` |
| Build logs | `~/.local/share/ekaci/logs` | `logs_dir` |
| Unix socket | `$XDG_RUNTIME_DIR/ekaci.sock` | `socket_path` |

For a multi-user system service you typically want these under `/var/lib/ekaci` and
`/var/log/ekaci`. See [Server Configuration](./server-configuration.md).

## Verify the install

Once the server is running you can ping it via the CLI:

```bash
ekaci status
```

And confirm the metrics endpoint is reachable:

```bash
curl http://127.0.0.1:3030/metrics | head
```

If both succeed, continue to [GitHub App Setup](./github-app-setup.md).
