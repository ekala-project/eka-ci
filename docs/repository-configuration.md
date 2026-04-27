# Repository Configuration

Repositories opt in to Eka CI by adding a `.eka-ci/config.json` file. This file is
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
      "allow_network": false,
      "ro_bind": ["/path/to/readonly/bind"]
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
| `shell` | yes | Attribute name of a shell derivation that provides the tools. |
| `command` | yes | The command line to run inside the sandbox. |
| `allow_network` | no | Default `false`. When `true`, the check is allowed network access. |
| `ro_bind` | no | Additional read-only bind mounts to expose to the sandbox. |

Checks are sandboxed via `birdcage` with no filesystem write access outside their working
directory and no network access by default. See
[Architecture](./architecture.md#security-model) for details on the security model.

## Cache references

The `caches` field on a job lists **cache IDs** — string identifiers from the server's
`[[caches]]` blocks. The repository never sees the underlying credentials, destinations, or
permissions.

If a job references a cache it is not allowed to push to (per the cache's
`allowed_repos`/`allowed_branches`), the push is silently skipped and a warning is logged.
The build itself still succeeds. See [Configuring Caches](./configure-caches.md).
