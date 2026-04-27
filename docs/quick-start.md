# Quick Start

This page walks through the minimum set of steps required to get Eka CI watching a single
repository. For deeper detail on each step, follow the linked pages.

## Prerequisites

- Nix package manager installed (with flakes enabled)
- A GitHub organization with admin access
- A publicly reachable HTTPS endpoint for receiving webhooks

## 1. Create a GitHub App

Eka CI authenticates to GitHub as a GitHub App. Create one at
`https://github.com/organizations/YOUR_ORG/settings/apps` with:

- **Permissions**: Checks (read/write), Contents (read), Pull Requests (read)
- **Events**: `pull_request`, `workflow_run`, `merge_group`, `installation`

Generate and download the private key. The full walkthrough, including all eight credential
sources, lives in [GitHub App Setup](./github-app-setup.md).

## 2. Configure the server

Create `~/.config/ekaci/ekaci.toml`:

```toml
[[github_apps]]
id = "main"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }

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

See [Server Configuration](./server-configuration.md) and
[Configuring Caches](./configure-caches.md) for the full set of options.

## 3. Configure the repository

Add `.eka-ci/config.json` at the root of any repository you want Eka CI to build:

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

See [Repository Configuration](./repository-configuration.md) for the full schema.

## 4. Run the server

```bash
nix build
./result/bin/eka-ci-server
```

For a long-running deployment, run it under systemd. A minimal unit file is included in
[Installation](./installation.md).

## 5. Open a pull request

Once the server is running and the GitHub App is installed on a repository, opening a pull
request will trigger:

1. An evaluation of the repository against the PR head and base.
2. A diff of derivations and a queued build for the changes.
3. One or more check runs reporting build status.
4. A `EkaCI: Change Summary` check posting a per-PR summary of changed packages and rebuild
   impact (see [Change Summaries](./change-summaries.md)).

From there, reviewers can merge through the GitHub UI or by commenting `@eka-ci merge` —
see [PR Comment Commands](./pr-commands.md).
