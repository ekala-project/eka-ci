# Gitea Setup Guide

This guide explains how to set up EkaCI with Gitea (self-hosted).

## Prerequisites

- EkaCI server running (see [Installation](installation.md))
- Gitea instance (self-hosted)
- Admin or owner access to your Gitea repository

## Overview

EkaCI integrates with Gitea through:
1. **Webhooks** - Gitea sends events (pull requests, pushes) to EkaCI
2. **Check Runs API** (Gitea v1.13+) or **Commit Status API** (older versions)
3. **Access Tokens** - Authentication for API calls

Gitea uses a **GitHub-compatible API**, so integration is similar to GitHub but adapted for self-hosted environments.

## Step 1: Create an Access Token

Gitea uses personal access tokens or application tokens for API authentication.

### 1.1: Generate Token

1. Log into your Gitea instance
2. Go to **Settings** → **Applications**
3. Scroll to **Generate New Token**
4. Configure the token:
   - **Token Name**: `EkaCI`
   - **Select permissions**:
     - ☑ `repo` - Full repository access
     - ☑ `write:status` - Update commit statuses
     - ☑ `read:user` - Read user information
5. Click **Generate Token**
6. **Important**: Copy the token immediately!

### Token Format

The token will look like:
```
a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0
```

## Step 2: Determine Gitea Version

Check your Gitea version to understand which API features are available:

```bash
curl https://gitea.example.com/api/v1/version
```

Response:
```json
{
  "version": "1.21.3"
}
```

**API Features by Version:**
- **v1.13+**: Check Runs API supported (GitHub-compatible)
- **< v1.13**: Commit Status API only

EkaCI automatically detects and uses the appropriate API.

## Step 3: Configure EkaCI Server

Add your Gitea instance configuration to EkaCI.

### Option A: Environment Variables

```bash
export GITEA_TOKEN="a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0"
export GITEA_DOMAIN="gitea.example.com"
```

### Option B: Configuration File

Add to your `config.toml`:

```toml
[gitea]
token = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0"
domain = "gitea.example.com"
# Optional: Force use of commit statuses instead of check runs
force_commit_status = false
```

### Multiple Gitea Instances

You can configure multiple Gitea instances:

```toml
[[gitea.instances]]
domain = "gitea.example.com"
token = "token-for-gitea-example-com"

[[gitea.instances]]
domain = "code.company.net"
token = "token-for-code-company-net"
```

## Step 4: Configure Webhook

Configure Gitea to send events to your EkaCI server.

### 4.1: Generate a Webhook Secret

Generate a random secret for webhook verification:

```bash
openssl rand -hex 32
```

Save this secret - you'll need it for both Gitea and EkaCI.

### 4.2: Configure EkaCI

Add the webhook secret to your EkaCI configuration:

```bash
export WEBHOOK_SECRET="your-generated-secret"
```

Or in `config.toml`:

```toml
[security]
webhook_secret = "your-generated-secret"
```

### 4.3: Configure Gitea Webhook

1. Navigate to your repository: `https://gitea.example.com/owner/repo`
2. Go to **Settings** → **Webhooks**
3. Click **Add Webhook** → **Gitea**
4. Configure the webhook:
   - **Target URL**: `https://your-ekaci-server.com/gitea/webhook`
   - **HTTP Method**: `POST`
   - **POST Content Type**: `application/json`
   - **Secret**: Paste the secret from step 4.1
   - **Trigger On**: Select events:
     - ☑ Push Events
     - ☑ Pull Request
     - ☑ Pull Request Comment
     - ☑ Issue Comment (for commands)
   - **Active**: ☑ Active
5. Click **Add Webhook**

### 4.4: Test the Webhook

1. Click on your newly created webhook
2. Scroll down to **Recent Deliveries**
3. Click **Test Delivery** → **Push Event**
4. Check the response:
   - **HTTP 204** = Success! ✅
   - **HTTP 401** = Authentication failed (check secret)
   - **HTTP 503** = EkaCI not configured

## Step 5: Create Repository Configuration

In your Gitea repository, create `.ekaci/config.json`:

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

  docker-image = pkgs.dockerTools.buildImage {
    name = "my-app";
    contents = [ my-app ];
  };
}
```

## Step 6: Test Integration

### Create a Test Pull Request

1. Create a new branch: `git checkout -b test-ekaci`
2. Make a small change to trigger CI
3. Push: `git push origin test-ekaci`
4. Create a pull request

### Verify CI is Working

You should see:
1. **Commit status** or **Check runs** appear on your PR
2. **EkaCI server logs** show webhook received:
   ```
   INFO webhook_received platform=gitea repo=owner/repo event_type=pull_request
   ```
3. **Build starts** in EkaCI

## Gitea-Specific Features

### Pull Request Events

EkaCI responds to these Gitea PR events:
- **opened** - Start CI for new PRs
- **synchronize** - Rebuild when commits are added
- **reopened** - Restart CI when PR is reopened
- **closed** - Clean up resources (optional)

### Status Reporting

#### Check Runs API (Gitea v1.13+)

For newer Gitea versions, EkaCI uses the Check Runs API (GitHub-compatible):
- More detailed status information
- Can include annotations and summaries
- Better UI integration

Example check run:
```
✓ EkaCI: all-packages (12 packages built successfully)
  Duration: 3m 42s
  View logs →
```

#### Commit Status API (Older Versions)

For Gitea < v1.13, EkaCI falls back to Commit Status API:
- Simple success/failure/pending states
- Less detailed information
- Compatible with all Gitea versions

Status states:
- **pending** - Build queued
- **success** - Build passed
- **failure** - Build failed
- **error** - Build error (internal)

### Change Summary Comments

When enabled, EkaCI posts a comment on your PR with:
- List of packages affected by changes
- Rebuild impact analysis
- Direct links to build logs

Example comment:
```markdown
## EkaCI Change Summary

**Packages affected**: 8 packages will be rebuilt

### Direct changes (2):
- `my-app` - Source files modified
- `tests` - Test configuration changed

### Transitive rebuilds (6):
- `docker-image` (depends on my-app)
- `integration-tests`
- ...

[View full report](https://ekaci.example.com/commits/abc123/rebuild-impact)
```

### Comment Commands

Post commands in PR comments to control EkaCI:

```
/ekaci merge
```

Supported commands:
- `/ekaci merge` - Merge PR after all checks pass
- `/ekaci retry` - Retry failed builds
- `/ekaci cancel` - Cancel running builds
- `/ekaci status` - Show current build status

## Troubleshooting

### Webhook not receiving events

**Check webhook configuration**:
1. Go to repository Settings → Webhooks
2. Click on your webhook
3. Check **Recent Deliveries** for errors

**Common issues**:
- **Connection refused**: EkaCI server not accessible from Gitea
- **SSL certificate problem**: Check certificate validity
- **401 Unauthorized**: Secret token mismatch
- **Firewall blocking**: Ensure Gitea can reach EkaCI

**Test connectivity from Gitea server**:
```bash
curl -X POST https://your-ekaci-server.com/gitea/webhook \
  -H "X-Gitea-Token: your-secret" \
  -H "X-Gitea-Event: push" \
  -d '{}'
```

### Builds not starting

**Check EkaCI logs**:
```bash
journalctl -u ekaci -f | grep gitea
```

Look for:
- Webhook parsing errors
- Database errors
- Nix evaluation failures

**Verify configuration**:
```bash
# Check token is set
echo $GITEA_TOKEN

# Test token validity
curl -H "Authorization: token $GITEA_TOKEN" \
  https://gitea.example.com/api/v1/user
```

### Status not appearing on commits

**Check Gitea version**:
```bash
# Get version
curl https://gitea.example.com/api/v1/version

# If < v1.13, check commit statuses endpoint
curl -H "Authorization: token $GITEA_TOKEN" \
  "https://gitea.example.com/api/v1/repos/owner/repo/commits/abc123/statuses"
```

**Verify API permissions**:
- Token has `write:status` permission
- Repository is accessible with the token

**Check for API rate limits**:
Gitea may rate-limit API calls. Check Gitea server logs:
```bash
# On Gitea server
tail -f /var/log/gitea/gitea.log | grep rate.limit
```

### Change summaries not posting

**Check configuration**:
```json
{
  "changeSummary": {
    "enabled": true  // Must be true
  }
}
```

**Verify comment permissions**:
- Token has `repo` permission
- EkaCI can post comments (check Gitea permissions)

## Self-Hosted Gitea

### Firewall Configuration

Ensure network connectivity:

1. **Outbound HTTPS** - Gitea → EkaCI webhook endpoint (port 443)
2. **Inbound API access** - EkaCI → Gitea API (port 3000 or custom)

### Network Topology

```
┌──────────────┐         HTTPS          ┌──────────────┐
│    Gitea     │ ───── Webhook ──────→  │     EkaCI    │
│  (gitea.    │                         │   Server     │
│  example.   │  ←──── API calls ─────  │              │
│   com:3000) │         HTTPS           │              │
└──────────────┘                         └──────────────┘
```

### Reverse Proxy Configuration

If Gitea is behind a reverse proxy (nginx, Caddy):

**Nginx example**:
```nginx
server {
    listen 443 ssl;
    server_name gitea.example.com;

    location / {
        proxy_pass http://localhost:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Ensure `X-Forwarded-*` headers are preserved for webhook IP tracking.

### Docker Deployment

If running Gitea in Docker:

**docker-compose.yml**:
```yaml
version: "3"
services:
  gitea:
    image: gitea/gitea:latest
    ports:
      - "3000:3000"
      - "222:22"
    environment:
      - GITEA__webhook__ALLOWED_HOST_LIST=*  # Allow webhooks to any host
      # Or restrict to EkaCI:
      # - GITEA__webhook__ALLOWED_HOST_LIST=ekaci.example.com
```

## Advanced Features

### Multiple Repositories

EkaCI can monitor multiple Gitea repositories:

1. Create webhook on each repository (use same secret)
2. Configure `.ekaci/config.json` per repository
3. EkaCI automatically routes events to the correct handler

### Organization-Wide Webhooks

For Gitea v1.13+, you can create organization webhooks:

1. Go to Organization **Settings** → **Webhooks**
2. Add webhook (same configuration as repository webhook)
3. All repos in the organization will trigger EkaCI

### Branch Protection

Combine EkaCI with Gitea's branch protection:

1. Go to **Settings** → **Branches**
2. Add protection rule for `main`:
   - ☑ Require status checks to pass
   - Select `EkaCI` from status checks
   - ☑ Require pull request reviews

### API Version Detection

EkaCI automatically detects Gitea API capabilities:

```rust
// Pseudocode
if gitea_version >= "1.13.0" {
    use_check_runs_api();
} else {
    use_commit_status_api();
}
```

To force commit status API (bypass auto-detection):

```toml
[gitea]
force_commit_status = true
```

## Migration from GitHub

If migrating from GitHub to Gitea:

### 1. Export GitHub repository
```bash
# Use Gitea's migration tool
gitea admin create-repo --migrate --from github \
  --repo owner/repo --owner gitea-user
```

### 2. Update EkaCI configuration
```toml
# Remove GitHub config
# [github]
# ...

# Add Gitea config
[gitea]
domain = "gitea.example.com"
token = "..."
```

### 3. Reconfigure webhooks

Follow [Step 4](#step-4-configure-webhook) to set up Gitea webhooks.

### 4. Verify repository configuration

`.ekaci/config.json` is compatible between GitHub and Gitea - no changes needed!

## Performance Optimization

### Webhook Processing

For high-volume repositories:

```toml
[gitea]
# Increase webhook concurrency
webhook_workers = 4

# Buffer size for event queue
webhook_buffer_size = 500
```

### API Rate Limits

Monitor API usage:
```bash
# Check EkaCI metrics
curl https://ekaci.example.com/v1/metrics | grep gitea_api
```

Gitea default limits (configurable in `app.ini`):
```ini
[api]
; Max requests per minute per token
DEFAULT_MAX_BLOB_SIZE = 10485760
MAX_RESPONSE_ITEMS = 50
```

## Next Steps

- [Repository Configuration](repository-configuration.md) - Configure individual repos
- [Multi-Platform Architecture](multi-platform-architecture.md) - Understand the design
- [Monitoring](monitoring.md) - Set up metrics and alerts

## Support

For issues specific to Gitea integration:
- Check [Troubleshooting](#troubleshooting) above
- Review EkaCI server logs
- Check Gitea version compatibility
- Open an issue: https://github.com/anthropics/eka-ci/issues

## Resources

- [Gitea Documentation](https://docs.gitea.io/)
- [Gitea API Reference](https://docs.gitea.io/en-us/api-usage/)
- [Gitea Webhooks](https://docs.gitea.io/en-us/webhooks/)
