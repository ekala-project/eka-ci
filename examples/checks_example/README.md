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

1. **Environment Setup**: The system obtains the Nix environment in one of two ways:
   - **Flake mode** (default): Runs `nix develop .#<shell> --command env` to get the devShell environment from flake.nix
   - **shell.nix mode**: Runs `nix-shell shell.nix -A <shell> --run env` to get the environment from shell.nix
2. **Repository Checkout**: A temporary copy of the repository is created
3. **Sandbox Creation**: Using birdcage, a sandbox is created with:
   - Read-only access to `/nix/store`
   - Read-write access to the checkout directory
   - Read-only access to `.git` directory
   - Optional network access
4. **Command Execution**: The command runs in the sandbox with the Nix environment
5. **Result Capture**: Exit code, stdout, stderr, and duration are recorded

## Security Features

- **Isolated Filesystem**: Commands can only access the checkout and `/nix/store`
- **Network Control**: Network access can be disabled per-check
- **No Nix Daemon**: The sandbox doesn't have access to the Nix daemon
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
