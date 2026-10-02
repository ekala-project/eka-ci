# Repository Configuration

Repositories opt in to Eka CI by adding a `.ekaci/config.json` file. This file is
**untrusted**: it can reference caches, jobs, and checks defined on the server, but it can
never inject credentials, host paths, or arbitrary commands beyond what the server allows.

## Schema

```json
{
  "jobs": {
    "package-name": {
      "file": "path/to/file.nix",
      "attr_path": "optional.attr.path",
      "allow_eval_failures": false,
      "caches": ["cache-id-from-server-config"],
      "size_check": {
        "max_increase_percent": 10.0,
        "base_branch": "main"
      }
    }
  },
  "checks": {
    "check-name": {
      "shell": "shell-derivation-attr",
      "command": "command to run",
      "allow_network": false
    }
  }
}
```

## Jobs

A *job* describes a Nix expression to evaluate and the derivations to build from it.

| Field | Required | Description |
|---|---|---|
| `file` | yes | Path to a `.nix` file relative to the repository root. |
| `attr_path` | no | Optional sub-attribute path inside the file. |
| `allow_eval_failures` | no | If `true`, evaluation errors do not fail the check. |
| `caches` | no | List of cache IDs (defined server-side) to push successful builds to. |
| `size_check` | no | Configures output- and closure-size monitoring. |

### Size checks

When `size_check` is set, Eka CI:

1. Calculates output (NAR) and closure size for each successful build.
2. Stores sizes in historical tables, keyed by commit and repository.
3. Compares against the most recent successful build on `base_branch`.
4. Logs warnings (and surfaces them in the change summary) when the increase exceeds
   `max_increase_percent`.

## Checks

A *check* runs a sandboxed command in a shell derivation defined in the repository.

| Field | Required | Description |
|---|---|---|
| `shell` | no | Dev shell providing the tools: `devShells.<system>.<shell>` of `flake.nix`, or attribute `<shell>` of `shell.nix`. The default shell when omitted. |
| `shell_nix` | no | Default `false`. Use `shell.nix` (`nix-shell`) instead of `flake.nix` (`nix develop`). |
| `command` | yes | The command line to run inside the sandbox. |
| `allow_network` | no | Default `false`. When `true`, the check command gets filtered internet access (no direct access to the server or its private networks). The dev shell capture always has it. |

Checks run in a bubblewrap + landlock sandbox with no filesystem write access outside
their checkout, only the dev shell's environment, and no direct network access by default.
The dev shell capture (`nix develop` / `nix-shell`, which evaluates the repository's
`flake.nix` or `shell.nix`) runs before the check in the same sandbox and always has
filtered network access, to fetch flake inputs; `allow_network` only controls the check
command.
With `allow_network`, network access is filtered server-side to the internet only, so a pull
request enabling it gains no direct access to the server or its private networks. Builds
requested through the Nix daemon use the daemon's network either way; see the caveat in
[Server Configuration](./server-configuration.md#sandbox) and the security model in
[Architecture](./architecture.md#security-model).

### Running checks locally

`ekaci check run [--check NAME]` runs the checks of the current repository sandboxed:

- **Linux**: the same sandbox as the server (needs `bwrap` and `pasta` on `PATH`).
  `--network-allow CIDR` (repeatable) opens a private IPv4 prefix to checks with
  `allow_network` and to the dev shell capture.
  An existing `.git` is read-only. In a directory without one, a `.git` the check creates
  is deleted when the check exits, so host `git` never runs hooks or config planted by it.
- **macOS**: Seatbelt (`sandbox-exec`). Reads are limited to `/nix/store`, a few system
  paths and the checkout, writes to the checkout (not `.git`). Seatbelt cannot filter by
  address, so `allow_network` there allows outbound traffic to everything except
  localhost (private networks included), and so does the dev shell capture of every
  check; a warning is printed. File metadata (`stat`)
  stays visible everywhere; `/etc/nix` is not readable.
- On macOS and with `--no-sandbox`, `--timeout` kills the check's process group; a process
  that leaves it (`setsid`) keeps running after `ekaci` exits. Only Linux confines the whole
  process tree (pid namespace).

`--timeout SECS` (default 1800) limits each check command; `--no-sandbox` runs checks
directly on the host.

## Cache references

The `caches` field on a job lists **cache IDs** — string identifiers from the server's
`[[caches]]` blocks. The repository never sees the underlying credentials, destinations, or
permissions.

If a job references a cache it is not allowed to push to (per the cache's
`allowed_repos`/`allowed_branches`), the push is silently skipped and a warning is logged.
The build itself still succeeds. See [Configuring Caches](./configure-caches.md).
