# Checks Example

This example demonstrates the new "checks" feature in eka-ci, which allows running sandboxed imperative commands on repository checkouts.

## Overview

Unlike "jobs" which evaluate Nix expressions to produce derivations, "checks" execute commands in a sandboxed environment with specific packages available. This is similar to GitHub Actions workflows but with enhanced security through sandboxing.

## Configuration

Checks are defined in `.ekaci/config.json`:

```json
{
  "checks": {
    "nixfmt": {
      "shell": "formatting",
      "command": "nixfmt --check **/*.nix",
      "allow_network": false
    },
    "echo-test": {
      "command": "echo 'Hello from sandboxed check!'",
      "allow_network": false
    },
    "network-test": {
      "shell": "networking",
      "command": "curl -I https://example.com",
      "allow_network": true
    }
  }
}
```

### Check Configuration Fields

- **shell**: Optional name of the shell environment to use
  - When `shell_nix` is false (default): References a flake devShell (e.g., "formatting" → `nix develop .#formatting`)
  - When `shell_nix` is true: References a shell.nix attribute (e.g., "myEnv" → `nix-shell shell.nix -A myEnv`)
  - If omitted: Uses default shell (`nix develop .` or `nix-shell shell.nix`)
- **shell_nix**: Boolean flag to use shell.nix instead of flake.nix (default: false)
  - `false`: Use `nix develop` with flake.nix (recommended for new projects)
  - `true`: Use `nix-shell` with shell.nix (for legacy compatibility)
- **command**: Shell command to execute in the sandboxed checkout
- **allow_network**: Boolean flag to enable/disable network access (default: false)

## How It Works

1. **Repository Checkout**: The server clones the commit into a temporary directory
2. **Environment Setup**: Inside the sandbox (filtered network), the system obtains the Nix environment in one of two ways:
   - **Flake mode** (default): Runs `nix develop .#<shell> --command env -0` to get the devShell environment from flake.nix
   - **shell.nix mode**: Runs `nix-shell shell.nix -A <shell> --run 'env -0'` to get the environment from shell.nix
3. **Sandbox Creation**: A bubblewrap + landlock sandbox is created with:
   - Read-only access to `/nix/store`
   - Read-write access to the checkout directory
   - Read-only access to `.git` directory
   - No network, or filtered internet access with `allow_network` (no private ranges, no host)
4. **Command Execution**: The command runs in the sandbox with only the dev shell's environment, under a timeout and memory limit
5. **Result Capture**: Exit code, stdout, stderr, and duration are recorded

## Security Features

- **Isolated Filesystem**: Commands can only access the checkout and `/nix/store`
- **Network Control**: No direct network by default; `allow_network` gives internet egress only, with no direct access to the server or its private networks
- **Nix Daemon**: The daemon socket is exposed, so `nix build` works; builds run in Nix's own sandbox with the daemon's network (fixed-output derivations can reach anything the daemon can)
- **Ephemeral Execution**: The checkout is discarded after the check completes

## Use Cases

- **Formatters**: `nixfmt`, `rustfmt`, `prettier`
- **Linters**: `statix`, `clippy`, `eslint`
- **Tests**: `cargo test`, `pytest`, `npm test`
- **Security Scans**: `cargo audit`, `npm audit`
- **Custom Scripts**: Any command that can be packaged with Nix

## Comparison with Jobs

| Feature | Jobs | Checks |
|---------|------|--------|
| Purpose | Build Nix derivations | Run imperative commands |
| Evaluation | Pure Nix evaluation | Sandboxed command execution |
| Caching | Nix store caching | No caching |
| Network | Depends on derivation | Configurable per-check |
| Use Case | Package builds | Linting, formatting, testing |
