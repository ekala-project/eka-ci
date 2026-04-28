# GitLab Setup Guide

This guide explains how to set up EkaCI with GitLab (self-hosted or GitLab.com).

## Prerequisites

- EkaCI server running (see [Installation](installation.md))
- GitLab account with project access
- Admin access to your GitLab project (to configure webhooks)

## Overview

EkaCI integrates with GitLab through:
1. **Webhooks** - GitLab sends events (merge requests, pushes) to EkaCI
2. **Commit Status API** - EkaCI reports build status back to GitLab
3. **Project Access Tokens** - Authentication for API calls

## Step 1: Create a Project Access Token

GitLab uses Project Access Tokens for API authentication.

### On GitLab.com or Self-Hosted GitLab:

1. Navigate to your project: `https://gitlab.com/your-org/your-repo`
2. Go to **Settings** → **Access Tokens**
3. Click **Add new token**
4. Configure the token:
   - **Token name**: `EkaCI`
   - **Role**: `Developer` or `Maintainer`
   - **Scopes**: Select:
     - ☑ `api` - Full API access
     - ☑ `read_repository` - Read repository
     - ☑ `write_repository` - Update commit statuses
5. Click **Create project access token**
6. **Important**: Copy the token immediately - you won't be able to see it again!

### Token Format

The token will look like:
```
glpat-xxxxxxxxxxxxxxxx
```

## Step 2: Configure EkaCI Server

Add your GitLab token to the EkaCI server configuration.

### Option A: Environment Variable (Single Instance)

```bash
export GITLAB_TOKEN="glpat-xxxxxxxxxxxxxxxx"
export GITLAB_DOMAIN="gitlab.com"  # Or your self-hosted domain
```

### Option B: Configuration File (TOML)

Add to your `ekaci.toml`:

```toml
[[gitlab_instances]]
domain = "gitlab.com"  # Or "gitlab.example.com" for self-hosted
token = "glpat-xxxxxxxxxxxxxxxx"
```

### Option C: NixOS Module

If using the NixOS module, configure via `services.eka-ci`:

```nix
services.eka-ci = {
  enable = true;
  environmentFile = "/run/secrets/eka-ci.env";

  settings.gitlab_instances = [{
    domain = "gitlab.com";
    token = null;  # Provided via environmentFile
  }];
};
```

Then in `/run/secrets/eka-ci.env`:
```bash
GITLAB_TOKEN=glpat-xxxxxxxxxxxxxxxx
```

See the [NixOS Module documentation](nixos-module.md#settingsgitlab_instances) for more details.

### Multiple GitLab Instances

You can configure multiple GitLab instances:

**TOML:**
```toml
[[gitlab_instances]]
domain = "gitlab.com"
token = "glpat-aaaaaaaaaaaaaa"

[[gitlab_instances]]
domain = "gitlab.example.com"
token = "glpat-bbbbbbbbbbbbbb"
```

**NixOS:**
```nix
settings.gitlab_instances = [
  { domain = "gitlab.com"; token = null; }
  { domain = "gitlab.example.com"; token = null; }
];
```

## Step 3: Configure Webhook

Configure GitLab to send events to your EkaCI server.

### 3.1: Generate a Webhook Secret

Generate a random secret for webhook verification:

```bash
openssl rand -hex 32
```

Save this secret - you'll need it for both GitLab and EkaCI.

### 3.2: Configure EkaCI

Add the webhook secret to your EkaCI configuration:

```bash
export WEBHOOK_SECRET="your-generated-secret"
```

Or in `config.toml`:

```toml
[security]
webhook_secret = "your-generated-secret"
```

### 3.3: Configure GitLab Webhook

1. Navigate to your GitLab project
2. Go to **Settings** → **Webhooks**
3. Click **Add new webhook**
4. Configure the webhook:
   - **URL**: `https://your-ekaci-server.com/gitlab/webhook`
   - **Secret token**: Paste the secret from step 3.1
   - **Trigger**: Select events:
     - ☑ Push events
     - ☑ Merge request events
     - ☑ Comments (for command processing)
   - **SSL verification**: ☑ Enable SSL verification (recommended)
5. Click **Add webhook**

### 3.4: Test the Webhook

1. Scroll down to your newly created webhook
2. Click **Test** → **Push events**
3. Check the response:
   - **HTTP 204** = Success! ✅
   - **HTTP 401** = Authentication failed (check secret)
   - **HTTP 503** = EkaCI not configured properly

## Step 4: Create Repository Configuration

In your GitLab repository, create `.ekaci/config.json`:

```json
{
  "jobs": {
    "default": {
      "nixFile": "ci.nix"
    }
  },
  "changeSummary": {
    "enabled": true,
    "maxPackages": 50
  }
}
```

Example `ci.nix`:

```nix
{ pkgs ? import <nixpkgs> {} }:

{
  # Your build jobs
  my-app = pkgs.callPackage ./default.nix {};

  tests = pkgs.runCommand "tests" {
    buildInputs = [ my-app ];
  } ''
    my-app --self-test
    touch $out
  '';
}
```

## Step 5: Test Integration

### Create a Test Merge Request

1. Create a new branch: `git checkout -b test-ekaci`
2. Make a small change to trigger CI
3. Push: `git push origin test-ekaci`
4. Create a merge request

### Verify CI is Working

You should see:
1. **Commit status** appears on your MR (pending → running → success/failure)
2. **EkaCI server logs** show webhook received:
   ```
   INFO webhook_received platform=gitlab repo=your-org/your-repo event_type=merge_request
   ```
3. **Build starts** in EkaCI

## GitLab-Specific Features

### Merge Request Events

EkaCI responds to these GitLab MR events:
- **opened** - Start CI for new MRs
- **synchronize** - Rebuild when commits are added
- **reopened** - Restart CI when MR is reopened

### Commit Status Reporting

GitLab uses the Commit Status API (not Check Runs like GitHub).

Status states:
- **pending** - Build queued
- **running** - Build in progress
- **success** - Build passed
- **failed** - Build failed

Statuses appear as icons next to commits and in the MR overview.

### Change Summary Comments

When enabled, EkaCI posts a comment on your MR with:
- List of packages affected by changes
- Rebuild impact analysis
- Direct links to build logs

Example comment:
```markdown
## EkaCI Change Summary

**Packages affected**: 12 packages will be rebuilt

### Direct changes (3):
- `my-app` - Source files modified
- `my-lib` - Dependencies updated
- `tests` - Test files changed

### Transitive rebuilds (9):
- `downstream-app`
- `integration-tests`
- ...

[View full report](https://ekaci.example.com/commits/abc123/rebuild-impact)
```

### Comment Commands

Post commands in MR comments to control EkaCI:

```
/ekaci merge
```

Supported commands:
- `/ekaci merge [method]` - Merge MR after all checks pass
  - Optional method: `merge`, `squash`, or `rebase` (defaults to server configuration)
  - Example: `/ekaci merge squash`
- `/ekaci retry` - Retry failed builds
- `/ekaci cancel` - Cancel running builds

### Auto-Merge and Merge Queue

EkaCI supports auto-merging MRs when all checks pass:

**How it works:**
1. Comment `/ekaci merge` (or `/ekaci merge squash`) on an MR
2. EkaCI validates permissions and merge method
3. When all CI checks pass, the MR is automatically merged
4. Uses the specified merge method (or default from server config)

**Merge methods supported:**
- `merge` - Creates a merge commit
- `squash` - Squashes all commits into one
- `rebase` - Rebases and fast-forwards

**Configuration:**
```toml
# In ekaci.toml
default_merge_method = "squash"  # Default: squash
```

**NixOS:**
```nix
settings.default_merge_method = "squash";  # One of: merge, squash, rebase
```

## Troubleshooting

### Webhook not receiving events

**Check webhook configuration**:
1. Go to Settings → Webhooks
2. Click Edit on your webhook
3. Scroll down to **Recent Deliveries**
4. Check for error messages

**Common issues**:
- **Connection refused**: EkaCI server not accessible from GitLab
- **SSL certificate problem**: Use valid SSL cert or disable SSL verification (not recommended)
- **401 Unauthorized**: Secret token mismatch

**Test connectivity**:
```bash
# From GitLab server (if self-hosted)
curl -X POST https://your-ekaci-server.com/gitlab/webhook \
  -H "X-Gitlab-Token: your-secret" \
  -H "X-Gitlab-Event: push" \
  -d '{}'
```

### Builds not starting

**Check EkaCI logs**:
```bash
journalctl -u ekaci -f
```

Look for:
- `ERROR` messages about webhook parsing
- Database errors
- Nix evaluation failures

**Verify configuration**:
```bash
# Check token is set
echo $GITLAB_TOKEN

# Test token validity
curl --header "PRIVATE-TOKEN: $GITLAB_TOKEN" \
  "https://gitlab.com/api/v4/user"
```

### Status not appearing on commits

**Verify API permissions**:
- Token has `api` and `write_repository` scopes
- Token role is `Developer` or higher

**Check API rate limits**:
GitLab rate limits API calls. If exceeded, status updates will fail.
- GitLab.com: 2,000 requests/min per token
- Self-hosted: Configurable by admin

### Change summaries not posting

**Check configuration**:
```json
{
  "changeSummary": {
    "enabled": true  // Must be true
  }
}
```

**Check logs**:
```bash
# Look for change summary generation errors
journalctl -u ekaci -f | grep change.summary
```

## Self-Hosted GitLab

### Additional Configuration

For self-hosted GitLab instances:

```toml
[gitlab]
domain = "gitlab.example.com"
token = "glpat-xxxxxxxxxxxxxxxx"
# Optional: Use HTTP instead of HTTPS (for testing only!)
use_https = false
# Optional: Skip SSL verification (not recommended for production)
verify_ssl = true
```

### Firewall Configuration

Ensure your self-hosted GitLab can reach EkaCI:

1. **Outbound HTTPS** - GitLab → EkaCI webhook endpoint
2. **Inbound API access** - EkaCI → GitLab API (port 443 or custom)

### Network Topology

```
┌──────────────┐         HTTPS          ┌──────────────┐
│   GitLab     │ ───── Webhook ──────→  │     EkaCI    │
│  (gitlab.   │                         │   Server     │
│  example.   │  ←──── API calls ─────  │              │
│   com)      │         HTTPS           │              │
└──────────────┘                         └──────────────┘
```

## Advanced Features

### Pipeline Integration

While EkaCI replaces GitLab CI for Nix builds, you can still use both:

**.gitlab-ci.yml** (optional):
```yaml
# Use GitLab CI for non-Nix tasks
lint:
  script:
    - npm run lint

# EkaCI handles Nix builds automatically
```

### Protected Branches

Configure protected branch rules in GitLab:
1. Go to Settings → Repository → Protected Branches
2. Protect `main` branch
3. Require "Passed" commit status from EkaCI

### Merge Request Approvals

Combine EkaCI with GitLab's approval system:
1. Settings → Merge Requests → Approval Rules
2. Require approvals AND passing EkaCI status

## Next Steps

- [Repository Configuration](repository-configuration.md) - Configure individual repos
- [Multi-Platform Architecture](multi-platform-architecture.md) - Understand the design
- [Monitoring](monitoring.md) - Set up metrics and alerts

## Support

For issues specific to GitLab integration:
- Check [Troubleshooting](#troubleshooting) above
- Review EkaCI server logs
- Open an issue: https://github.com/anthropics/eka-ci/issues
