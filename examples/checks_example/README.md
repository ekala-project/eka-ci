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
      "packages": ["nixfmt"],
      "command": "nixfmt --check **/*.nix",
      "allow_network": false
    },
    "echo-test": {
      "packages": ["coreutils"],
      "command": "echo 'Hello from sandboxed check!'",
      "allow_network": false
    }
  }
}
```

### Check Configuration Fields

- **packages**: Array of Nix package names to make available in the environment
- **command**: Shell command to execute in the sandboxed checkout
- **allow_network**: Boolean flag to enable/disable network access (default: false)

## How It Works

1. **Environment Setup**: The system runs `nix-shell -p <packages> --run 'env'` to get the Nix environment
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
