# GitHub App Setup and Configuration Guide

Complete guide for creating, configuring, and securing GitHub Apps for eka-ci.

## Table of Contents

- [Introduction](#introduction)
  - [What is a GitHub App?](#what-is-a-github-app)
  - [Why GitHub Apps?](#why-github-apps)
  - [Prerequisites](#prerequisites)
- [Part 1: Creating the GitHub App](#part-1-creating-the-github-app)
  - [Step 1: Navigate to GitHub App Settings](#step-1-navigate-to-github-app-settings)
  - [Step 2: Configure Basic Information](#step-2-configure-basic-information)
  - [Step 3: Configure Webhook Settings](#step-3-configure-webhook-settings)
  - [Step 4: Configure Permissions](#step-4-configure-permissions)
  - [Step 5: Subscribe to Events](#step-5-subscribe-to-events)
  - [Step 6: Installation Scope](#step-6-installation-scope)
  - [Step 7: Create the App](#step-7-create-the-app)
- [Part 2: Obtaining Credentials](#part-2-obtaining-credentials)
  - [App ID](#app-id)
  - [Private Key](#private-key)
- [Part 3: Securing Your Credentials](#part-3-securing-your-credentials)
  - [Development: Environment Variables](#development-environment-variables)
  - [Production Option 1: File-Based with Restricted Permissions](#production-option-1-file-based-with-restricted-permissions)
  - [Production Option 2: GitHub App Key File](#production-option-2-github-app-key-file)
  - [Production Option 3: HashiCorp Vault (Recommended)](#production-option-3-hashicorp-vault-recommended)
  - [Production Option 4: AWS Secrets Manager](#production-option-4-aws-secrets-manager)
  - [Production Option 5: systemd Credentials (Linux with TPM2)](#production-option-5-systemd-credentials-linux-with-tpm2)
  - [Production Option 6: Instance Metadata (Cloud VMs)](#production-option-6-instance-metadata-cloud-vms)
  - [Production Option 7: AWS Profile](#production-option-7-aws-profile)
- [Part 4: Installing the GitHub App](#part-4-installing-the-github-app)
  - [Step 1: Install on Your Organization](#step-1-install-on-your-organization)
  - [Step 2: Verify Installation](#step-2-verify-installation)
- [Part 5: Configuring eka-ci Server](#part-5-configuring-eka-ci-server)
  - [Basic Configuration](#basic-configuration)
  - [All Credential Source Options](#all-credential-source-options)
  - [Permission Controls](#permission-controls)
  - [Complete Configuration Examples](#complete-configuration-examples)
- [Part 6: Testing the Setup](#part-6-testing-the-setup)
  - [Test 1: Check Server Startup](#test-1-check-server-startup)
  - [Test 2: Create a Test Pull Request](#test-2-create-a-test-pull-request)
  - [Test 3: Verify Webhook Delivery](#test-3-verify-webhook-delivery)
- [Security Best Practices](#security-best-practices)
  - [Protect Your Private Key](#protect-your-private-key)
  - [Rotate Credentials Regularly](#rotate-credentials-regularly)
  - [Use Webhook Secrets](#use-webhook-secrets)
  - [Principle of Least Privilege](#principle-of-least-privilege)
  - [Monitor and Audit](#monitor-and-audit)
  - [Network Security](#network-security)
  - [Secure the Server](#secure-the-server)
- [Troubleshooting](#troubleshooting)
  - [GitHub App Registration Fails](#github-app-registration-fails)
  - [Webhooks Not Received](#webhooks-not-received)
  - [Permission Denied Errors](#permission-denied-errors)
  - [Check Runs Not Appearing](#check-runs-not-appearing)
  - [Private Key Format Errors](#private-key-format-errors)
  - [Vault Connection Fails](#vault-connection-fails)
  - [AWS Secrets Manager Fails](#aws-secrets-manager-fails)
  - [Multiple GitHub Apps Not Supported](#multiple-github-apps-not-supported)
- [Advanced Topics](#advanced-topics)
  - [Approval Workflow Integration](#approval-workflow-integration)
  - [Merge Queue Support](#merge-queue-support)
  - [OAuth Integration (Optional)](#oauth-integration-optional)
  - [Migration from Environment Variables](#migration-from-environment-variables)
- [API Reference](#api-reference)
  - [CredentialSource Enum](#credentialsource-enum)
  - [GitHubAppConfig Structure](#githubappconfig-structure)
- [FAQ](#faq)
- [Related Documentation](#related-documentation)

---

## Introduction

### What is a GitHub App?

A GitHub App is a first-class integration with GitHub that provides:
- Fine-grained permissions
- Webhook-based event delivery
- Organization-wide installation
- Higher API rate limits
- Better security than personal access tokens

### Why GitHub Apps?

eka-ci uses GitHub Apps because they:

- ✅ **Fine-grained permissions** - Request only the access you need
- ✅ **Organization-wide installation** - One setup for all repositories
- ✅ **Better security** - Credentials can't be used to access user data
- ✅ **Webhook integration** - Automatic notifications for CI events
- ✅ **Rate limit advantages** - Higher API rate limits (5,000 vs 1,000 requests/hour)

### Prerequisites

Before you begin:

1. **Administrative access** to the GitHub organization where you want to install eka-ci
2. **A running eka-ci server** with a publicly accessible URL (for webhooks)
3. **Access to secure credential storage** (Vault, AWS Secrets Manager, or similar for production)

---

## Part 1: Creating the GitHub App

### Step 1: Navigate to GitHub App Settings

1. Go to your organization's settings page:
   ```
   https://github.com/organizations/YOUR_ORG/settings/apps
   ```

   Or for personal accounts:
   ```
   https://github.com/settings/apps
   ```

2. Click **"New GitHub App"**

### Step 2: Configure Basic Information

Fill in the basic app information:

| Field | Value | Notes |
|-------|-------|-------|
| **GitHub App name** | `eka-ci` (or your preferred name) | Must be unique across GitHub |
| **Homepage URL** | `https://your-eka-ci-server.com` | Your eka-ci server's public URL |
| **Description** | `Continuous Integration for Nix projects` | Optional but recommended |
| **Callback URL** | Leave empty | Not used by eka-ci |
| **Setup URL** | Leave empty | Not used by eka-ci |

### Step 3: Configure Webhook Settings

This is **critical** for eka-ci to receive events:

| Field | Value | Notes |
|-------|-------|-------|
| **Webhook URL** | `https://your-eka-ci-server.com/github/webhook` | Must be publicly accessible |
| **Webhook secret** | Generate a strong secret | **IMPORTANT**: Save this securely! |

**Generating a webhook secret:**

```bash
# Generate a random secret
openssl rand -hex 32

# Example output:
# 3f8a9c7b2e1d6f4a8b9c7e2d1f6a4b9c8e7d2f1a6b4c9e8d7f2a1b6c4e9d8f7
```

⚠️ **Security Note**: The webhook secret verifies that webhook payloads come from GitHub. While eka-ci currently doesn't verify this signature (pending implementation), you should still configure it for future use.

**Webhook settings:**
- **Content type:** `application/json`
- **SSL verification:** ✅ Enable SSL verification (required for production)

### Step 4: Configure Permissions

eka-ci requires these **Repository permissions**:

| Permission | Access Level | Purpose |
|------------|--------------|---------|
| **Checks** | Read & Write | Create and update CI check runs on PRs |
| **Contents** | Read only | Clone repositories and read source code |
| **Pull requests** | Read only | Receive PR events and read PR metadata |
| **Metadata** | Read only | Default permission (automatically included) |

**Do NOT grant:**
- Write access to Contents, Pull Requests, or Issues (not needed)
- Any Organization permissions
- Any Account permissions

### Step 5: Subscribe to Events

Enable these webhook events:

- ✅ **Pull request** - Triggers builds on PR open, update, close
- ✅ **Workflow run** - For approval workflow integration
- ✅ **Merge group** - For GitHub merge queue support
- ✅ **Installation** - Tracks when app is installed/uninstalled
- ✅ **Installation repositories** - Tracks repository access changes

**Do NOT enable:**
- Push events (eka-ci is PR-focused)
- Issue events (not used)
- Other events (creates unnecessary webhook traffic)

### Step 6: Installation Scope

Choose **"Only on this account"** unless you plan to distribute eka-ci as a public service.

### Step 7: Create the App

1. Review your settings
2. Click **"Create GitHub App"**
3. You'll be redirected to your app's settings page

---

## Part 2: Obtaining Credentials

After creating the app, you need two pieces of information:

### App ID

1. On your GitHub App's settings page, find **"App ID"** near the top
2. It's a numeric value like `123456`
3. Save this - you'll need it for eka-ci configuration

### Private Key

1. Scroll down to the **"Private keys"** section
2. Click **"Generate a private key"**
3. A `.pem` file will download automatically
4. **CRITICAL**: Store this file securely - it cannot be recovered if lost!

The downloaded file looks like:

```
your-app-name.YYYY-MM-DD.private-key.pem
```

Contents:
```
-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA1234567890abcdefghijklmnopqrstuvwxyz...
...multiple lines of base64-encoded key data...
-----END RSA PRIVATE KEY-----
```

---

## Part 3: Securing Your Credentials

**⚠️ NEVER commit credentials to version control!**

eka-ci supports **8 different methods** for securing GitHub App credentials. Choose based on your environment:

### Development: Environment Variables

**Pros:** Simple, quick setup
**Cons:** Not suitable for production, credentials in memory

```bash
export GITHUB_APP_ID=123456
export GITHUB_APP_PRIVATE_KEY="$(cat your-app-name.private-key.pem)"
./eka-ci-server
```

**eka-ci configuration** (`~/.config/ekaci/ekaci.toml`):
```toml
# No configuration needed - automatic fallback to environment variables
# OR explicitly:
[[github_apps]]
id = "dev-app"
credentials = { env = { vars = ["GITHUB_APP_ID", "GITHUB_APP_PRIVATE_KEY"] } }
```

⚠️ **Not recommended for production!** Use one of the secure methods below.

### Production Option 1: File-Based with Restricted Permissions

**Pros:** Simple, no external dependencies
**Cons:** Credentials on disk, manual rotation

1. Create a secure directory:
```bash
sudo mkdir -p /etc/eka-ci
sudo chmod 700 /etc/eka-ci
```

2. Create a credentials file (JSON format):
```bash
sudo tee /etc/eka-ci/github-app.json <<EOF
{
  "GITHUB_APP_ID": "123456",
  "GITHUB_APP_PRIVATE_KEY": "$(cat your-app-name.private-key.pem | sed 's/$/\\n/' | tr -d '\n')"
}
EOF

sudo chmod 600 /etc/eka-ci/github-app.json
sudo chown eka-ci:eka-ci /etc/eka-ci/github-app.json
```

3. Configure eka-ci:
```toml
[[github_apps]]
id = "production"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }
```

**Supported file formats:**

**JSON:**
```json
{
  "GITHUB_APP_ID": "123456",
  "GITHUB_APP_PRIVATE_KEY": "-----BEGIN RSA PRIVATE KEY-----\n..."
}
```

**Key=value:**
```
GITHUB_APP_ID=123456
GITHUB_APP_PRIVATE_KEY=-----BEGIN RSA PRIVATE KEY-----
...
-----END RSA PRIVATE KEY-----
```

### Production Option 2: GitHub App Key File

**Pros:** Keeps private key in original PEM format
**Cons:** Credentials on disk

1. Store the private key securely:
```bash
sudo cp your-app-name.private-key.pem /etc/eka-ci/github-app-key.pem
sudo chmod 600 /etc/eka-ci/github-app-key.pem
sudo chown eka-ci:eka-ci /etc/eka-ci/github-app-key.pem
```

2. Configure eka-ci:
```toml
[[github_apps]]
id = "production"
credentials = { github-app-key-file = {
    app_id_env = "GITHUB_APP_ID",
    key_file = "/etc/eka-ci/github-app-key.pem"
}}
```

3. Set the App ID:
```bash
export GITHUB_APP_ID=123456
```

### Production Option 3: HashiCorp Vault (Recommended)

**Pros:** Best security, audit logs, automatic rotation
**Cons:** Requires Vault infrastructure

1. Store credentials in Vault:
```bash
# First, format the private key for JSON (escape newlines)
PRIVATE_KEY=$(cat your-app-name.private-key.pem | sed 's/$/\\n/' | tr -d '\n')

# Store in Vault
vault kv put secret/eka-ci/github-app \
  GITHUB_APP_ID="123456" \
  GITHUB_APP_PRIVATE_KEY="${PRIVATE_KEY}"
```

2. Configure eka-ci:
```toml
[[github_apps]]
id = "production"

[github_apps.credentials.vault]
address = "https://vault.example.com:8200"
secret_path = "eka-ci/github-app"
token_env = "VAULT_TOKEN"
namespace = "production"  # Optional, for Vault Enterprise
```

3. Run eka-ci with Vault token:
```bash
export VAULT_TOKEN=s.your-vault-token
./eka-ci-server
```

### Production Option 4: AWS Secrets Manager

**Pros:** Managed service, integrates with IAM
**Cons:** AWS-specific, costs money

1. Create secret in AWS Secrets Manager:
```bash
# Format the private key
PRIVATE_KEY=$(cat your-app-name.private-key.pem | sed 's/$/\\n/' | tr -d '\n')

# Create secret
aws secretsmanager create-secret \
  --name eka-ci/github-app \
  --description "eka-ci GitHub App credentials" \
  --secret-string "{\"GITHUB_APP_ID\":\"123456\",\"GITHUB_APP_PRIVATE_KEY\":\"${PRIVATE_KEY}\"}"
```

2. Configure eka-ci:
```toml
[[github_apps]]
id = "production"

[github_apps.credentials.aws-secrets-manager]
secret_name = "eka-ci/github-app"
region = "us-east-1"  # Optional, defaults to AWS_REGION env var
```

3. Ensure eka-ci has IAM permissions:
```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "secretsmanager:GetSecretValue",
      "Resource": "arn:aws:secretsmanager:us-east-1:ACCOUNT_ID:secret:eka-ci/github-app-*"
    }
  ]
}
```

### Production Option 5: systemd Credentials (Linux with TPM2)

**Pros:** Hardware-encrypted, no external dependencies
**Cons:** Requires systemd 250+, TPM2 chip

1. Create credential file:
```bash
# Format credentials as JSON
cat > /tmp/github-app.json <<EOF
{
  "GITHUB_APP_ID": "123456",
  "GITHUB_APP_PRIVATE_KEY": "$(cat your-app-name.private-key.pem | sed 's/$/\\n/' | tr -d '\n')"
}
EOF
```

2. Encrypt with systemd:
```bash
# Encrypt using TPM2
sudo systemd-creds encrypt \
  --name=github-app-credentials \
  /tmp/github-app.json \
  /var/lib/systemd/credential/github-app.cred

# Clean up plaintext
shred -u /tmp/github-app.json
```

3. Configure systemd service:
```ini
[Service]
LoadCredential=github-app-credentials:/var/lib/systemd/credential/github-app.cred
```

4. Configure eka-ci:
```toml
[[github_apps]]
id = "production"
credentials = { systemd-credential = { name = "github-app-credentials" } }
```

### Production Option 6: Instance Metadata (Cloud VMs)

**Pros:** No credentials on disk, automatic rotation
**Cons:** Requires IAM role setup, cloud-specific

For EC2/GCP/Azure instances with IAM roles that can access AWS Secrets Manager:

```toml
[[github_apps]]
id = "cloud-production"
credentials = "instance-metadata"
```

The instance profile must have permissions to access Secrets Manager (see Option 4).

### Production Option 7: AWS Profile

**Pros:** Uses existing AWS credentials
**Cons:** AWS-specific

Use credentials from `~/.aws/credentials`:

```toml
[[github_apps]]
id = "aws-profile-app"
credentials = { aws-profile = { profile = "eka-ci-production" } }
```

**~/.aws/credentials:**
```ini
[eka-ci-production]
aws_access_key_id = AKIAIOSFODNN7EXAMPLE
aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
```

---

## Part 4: Installing the GitHub App

### Step 1: Install on Your Organization

1. Go to your GitHub App's settings page
2. Click **"Install App"** in the left sidebar
3. Select your organization
4. Choose repository access:
   - **"All repositories"** - eka-ci will build all repos (recommended)
   - **"Only select repositories"** - Choose specific repos

5. Click **"Install"**

### Step 2: Verify Installation

eka-ci automatically tracks installations via webhooks. Check the logs:

```bash
# Look for installation confirmation
journalctl -u eka-ci -f | grep -i "installation"

# Expected output:
# INFO eka_ci_server::github::webhook: Received installation event: created
# INFO eka_ci_server::db: Stored GitHub installation: id=12345678
```

---

## Part 5: Configuring eka-ci Server

### Basic Configuration

Create or edit `~/.config/ekaci/ekaci.toml`:

```toml
# GitHub App configuration
[[github_apps]]
id = "main"

# Choose ONE credential source from Part 3
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }

# Permissions (optional, defaults to allow all)
[github_apps.permissions]
allow_all = true
```

### All Credential Source Options

Quick reference for all available credential sources:

```toml
# 1. Environment Variables
[[github_apps]]
id = "env-based"
credentials = { env = { vars = ["GITHUB_APP_ID", "GITHUB_APP_PRIVATE_KEY"] } }

# 2. File (JSON or key=value)
[[github_apps]]
id = "file-based"
credentials = { file = { path = "/etc/eka-ci/github-app.json" } }

# 3. GitHub App Key File
[[github_apps]]
id = "key-file"
credentials = { github-app-key-file = {
    app_id_env = "GITHUB_APP_ID",
    key_file = "/etc/eka-ci/github-app-key.pem"
}}

# 4. HashiCorp Vault
[[github_apps]]
id = "vault"
[github_apps.credentials.vault]
address = "https://vault.example.com:8200"
secret_path = "eka-ci/github-app"
token_env = "VAULT_TOKEN"
namespace = "production"  # Optional

# 5. AWS Secrets Manager
[[github_apps]]
id = "aws-sm"
[github_apps.credentials.aws-secrets-manager]
secret_name = "eka-ci/github-app"
region = "us-east-1"  # Optional

# 6. systemd Credentials
[[github_apps]]
id = "systemd"
credentials = { systemd-credential = { name = "github-app-credentials" } }

# 7. Instance Metadata
[[github_apps]]
id = "imds"
credentials = "instance-metadata"

# 8. AWS Profile
[[github_apps]]
id = "aws-profile"
credentials = { aws-profile = { profile = "eka-ci-production" } }
```

### Permission Controls

GitHub App permissions allow you to restrict which repositories and branches can use specific GitHub App credentials.

#### Allow All (Default)

```toml
[[github_apps]]
id = "unrestricted-app"
credentials = { /* ... */ }

[github_apps.permissions]
allow_all = true
```

#### Repository Restrictions

Only specific repositories can use this GitHub App:

```toml
[[github_apps]]
id = "restricted-app"
credentials = { /* ... */ }

[github_apps.permissions]
allow_all = false
allowed_repos = [
    "myorg/repo1",
    "myorg/repo2",
    "anotherorg/special-repo"
]
```

#### Branch Restrictions

Restrict to specific branches or branch patterns:

```toml
[[github_apps]]
id = "production-only-app"
credentials = { /* ... */ }

[github_apps.permissions]
allow_all = false
allowed_repos = ["myorg/*"]
allowed_branches = [
    "main",
    "master",
    "release/*",
    "hotfix/*"
]
```

**Glob pattern support:**
- `"main"` - exact match
- `"release/*"` - prefix match (e.g., release/v1.0, release/v2.0)
- `"*/staging"` - suffix match
- `"*"` - match all

### Complete Configuration Examples

#### Development Setup

Simple environment variable-based setup:

```toml
# ~/.config/ekaci/ekaci.toml

# No github_apps section needed - falls back to environment variables
# Set: GITHUB_APP_ID and GITHUB_APP_PRIVATE_KEY
```

Or explicitly:

```toml
[[github_apps]]
id = "dev"
credentials = { env = { vars = ["GITHUB_APP_ID", "GITHUB_APP_PRIVATE_KEY"] } }

[github_apps.permissions]
allow_all = true
```

#### Production with Vault

Multi-environment setup with Vault:

```toml
# Production app - restricted to production repos
[[github_apps]]
id = "production"

[github_apps.credentials.vault]
address = "https://vault.prod.example.com:8200"
secret_path = "eka-ci/github-app-prod"
token_env = "VAULT_TOKEN"
namespace = "production"

[github_apps.permissions]
allow_all = false
allowed_repos = ["company/production-*"]
allowed_branches = ["main", "release/*"]

# Staging app - restricted to staging repos
[[github_apps]]
id = "staging"

[github_apps.credentials.vault]
address = "https://vault.staging.example.com:8200"
secret_path = "eka-ci/github-app-staging"
token_env = "VAULT_TOKEN"
namespace = "staging"

[github_apps.permissions]
allow_all = false
allowed_repos = ["company/staging-*"]
allowed_branches = ["main", "develop", "feature/*"]
```

#### AWS-Based Production

Using AWS Secrets Manager with instance metadata:

```toml
[[github_apps]]
id = "aws-production"

[github_apps.credentials.aws-secrets-manager]
secret_name = "prod/eka-ci/github-app"
region = "us-east-1"

[github_apps.permissions]
allow_all = false
allowed_repos = ["mycompany/*"]
allowed_branches = ["main", "release/*"]
```

---

## Part 6: Testing the Setup

### Test 1: Check Server Startup

Start eka-ci and verify GitHub App registration:

```bash
./eka-ci-server

# Expected log output:
# INFO eka_ci_server::github: Registering GitHub App from configuration: main
# INFO eka_ci_server::github: Successfully registered as GitHub app
```

### Test 2: Create a Test Pull Request

1. Create a test branch in one of your repositories
2. Make a trivial change and open a PR
3. Check that eka-ci creates a check run on the PR

**What to look for:**
- A check run appears on the PR (usually named "eka-ci")
- Initial status is "Queued" or "In Progress"
- Check the eka-ci logs for webhook receipt:
  ```bash
  journalctl -u eka-ci -f | grep webhook

  # Expected:
  # INFO eka_ci_server::github::webhook: Received pull_request event: opened
  # INFO eka_ci_server::scheduler: Queued build for PR #123
  ```

### Test 3: Verify Webhook Delivery

On GitHub:

1. Go to your GitHub App's settings
2. Click **"Advanced"** tab
3. Scroll to **"Recent Deliveries"**
4. Verify webhooks are being delivered successfully (green checkmarks)
5. If you see red X's, click to view the error details

---

## Security Best Practices

### Protect Your Private Key

- ✅ **DO**: Store in a secret manager (Vault, AWS Secrets Manager)
- ✅ **DO**: Use file permissions 600 (owner read/write only)
- ✅ **DO**: Encrypt with TPM2 (systemd credentials)
- ❌ **DON'T**: Commit to Git
- ❌ **DON'T**: Store in Docker images
- ❌ **DON'T**: Share via chat/email
- ❌ **DON'T**: Log to files or stdout

### Rotate Credentials Regularly

**How to rotate the private key:**

1. Generate a new private key on GitHub App settings
2. Update the key in your secret manager
3. Restart eka-ci to load the new key
4. Delete the old key from GitHub

**Recommended rotation schedule:**
- Production: Every 90 days
- Staging: Every 180 days
- Development: Yearly

### Use Webhook Secrets

Configure a webhook secret and verify signatures in your eka-ci deployment.

⚠️ **Current Status**: Webhook signature verification is planned but not yet implemented in eka-ci. You should still configure a webhook secret for future use.

**To implement verification** (for contributors):
```rust
// In webhook handler, verify HMAC-SHA256 signature
let signature = headers.get("X-Hub-Signature-256");
let payload = request.body();
let expected = hmac_sha256(webhook_secret, payload);
assert_eq!(signature, expected);
```

### Principle of Least Privilege

Only grant the **minimum required permissions**:

- ✅ Checks: Read & Write (required)
- ✅ Contents: Read only (required)
- ✅ Pull Requests: Read only (required)
- ❌ **Never** grant write access to Contents, PRs, or Issues
- ❌ **Never** grant Organization or Account permissions

### Monitor and Audit

**Monitor webhook delivery:**
```bash
# Check for webhook failures
journalctl -u eka-ci | grep -i "webhook.*error"

# Monitor installation changes
journalctl -u eka-ci | grep -i "installation"
```

**Audit credential access:**
- Enable Vault audit logging
- Enable AWS CloudTrail for Secrets Manager
- Review systemd journal for credential loads

**Additional security practices:**
- Never commit credentials to version control
- Use `.gitignore` for credential files
- Always use secret management systems in production
- Create separate GitHub Apps for different environments
- Use permission restrictions to limit blast radius
- Enable audit logging in Vault/AWS
- Monitor who accesses GitHub App credentials
- Review permission configurations regularly
- Use instance metadata in cloud deployments to avoid storing long-lived credentials

### Network Security

**Webhook endpoint security:**
- ✅ Use HTTPS (required for production)
- ✅ Use a valid SSL certificate
- ✅ Configure firewall to allow GitHub IPs only (optional)
- ✅ Use webhook secrets when implemented

**GitHub IP ranges** (for firewall rules):
```bash
# Download GitHub's IP ranges
curl https://api.github.com/meta | jq -r '.hooks[]'

# Example firewall rule (iptables)
iptables -A INPUT -p tcp --dport 443 -s 192.30.252.0/22 -j ACCEPT
```

### Secure the Server

**Server hardening checklist:**
- ✅ Run eka-ci as non-root user
- ✅ Use systemd sandboxing features
- ✅ Enable SELinux or AppArmor
- ✅ Keep dependencies updated
- ✅ Enable automatic security updates
- ✅ Monitor logs for suspicious activity

---

## Troubleshooting

### GitHub App Registration Fails

**Error:** `failed to locate $GITHUB_APP_ID`

**Cause**: Credentials not properly configured

**Solution**:
1. Check your configuration file syntax
2. Verify the credentials file exists and has correct permissions
3. For Vault/AWS, verify connectivity and permissions
4. Check eka-ci logs for detailed error messages

If using the configuration file, ensure you have:
```toml
[[github_apps]]
id = "..."
credentials = { /* valid credential source */ }
```

### Webhooks Not Received

**Symptoms**: PRs don't trigger builds

**Debugging steps**:

1. **Verify webhook URL is correct:**
   ```bash
   curl https://your-eka-ci-server.com/github/webhook
   # Should return 405 Method Not Allowed (GET not supported)
   ```

2. **Check GitHub webhook deliveries:**
   - Go to GitHub App settings → Advanced → Recent Deliveries
   - Look for failed deliveries (red X)
   - Click to see error details

3. **Common webhook errors:**
   - **SSL certificate error**: Fix your SSL cert or disable verification (dev only)
   - **Timeout**: Server is slow or down
   - **Connection refused**: Firewall blocking GitHub IPs

4. **Check eka-ci logs:**
   ```bash
   journalctl -u eka-ci -n 100 | grep webhook
   ```

### Permission Denied Errors

**Error:** `Repository myorg/myrepo is not allowed to use GitHub App production`

**Solution:** Update permissions in config:
```toml
[github_apps.permissions]
allow_all = false
allowed_repos = ["myorg/myrepo", "myorg/*"]
```

Or if the issue is GitHub App permissions:

**Cause**: GitHub App doesn't have required permissions

**Solution**:
1. Go to GitHub App settings → Permissions
2. Verify:
   - Checks: Read & Write
   - Contents: Read
   - Pull Requests: Read
3. If you changed permissions, you must **reinstall** the app:
   - Go to Installations
   - Click "Configure"
   - Accept new permissions

### Check Runs Not Appearing

**Symptoms**: Webhook received but no check run created

**Debugging:**

1. Check logs for errors:
   ```bash
   journalctl -u eka-ci -f | grep -E "(check|error)"
   ```

2. Verify repository is configured:
   - Repository must have `.eka-ci/config.json`
   - Configuration must define jobs

3. Check GitHub API rate limits:
   ```bash
   # The eka-ci server logs should show rate limit status
   journalctl -u eka-ci | grep "rate limit"
   ```

### Private Key Format Errors

**Error**: "invalid value for $GITHUB_APP_PRIVATE_KEY"

**Cause**: Private key not properly formatted for storage

**Solution**:

For JSON files, escape newlines:
```bash
# Convert PEM to JSON-safe format
PRIVATE_KEY=$(cat key.pem | sed 's/$/\\n/' | tr -d '\n')
echo "{\"GITHUB_APP_PRIVATE_KEY\":\"${PRIVATE_KEY}\"}"
```

For environment variables, preserve newlines:
```bash
# Use actual newlines
export GITHUB_APP_PRIVATE_KEY="$(cat key.pem)"
```

### Vault Connection Fails

**Error:** `Failed to read secret from Vault path`

**Checklist:**
- Verify Vault address is correct
- Check VAULT_TOKEN environment variable is set
- Verify secret path exists: `vault kv get secret/eka-ci/github-app`
- Check Vault namespace if using enterprise Vault
- Ensure Vault token has read permissions

### AWS Secrets Manager Fails

**Error:** `Failed to retrieve secret from AWS Secrets Manager`

**Checklist:**
- Verify AWS credentials are configured (env vars or instance profile)
- Check secret name is correct
- Verify region setting
- Ensure IAM permissions include `secretsmanager:GetSecretValue`
- Check secret is in JSON format with correct keys

### Multiple GitHub Apps Not Supported

**Current Status**: eka-ci uses the first configured GitHub App for all repositories.

**Workaround**: Use permission restrictions to limit apps to specific repos:
```toml
[[github_apps]]
id = "prod"
credentials = { /* ... */ }

[github_apps.permissions]
allowed_repos = ["myorg/prod-*"]

[[github_apps]]
id = "dev"
credentials = { /* ... */ }

[github_apps.permissions]
allowed_repos = ["myorg/dev-*"]
```

⚠️ **Note**: Only the first app will be used currently. Multi-app support is planned.

---

## Advanced Topics

### Approval Workflow Integration

eka-ci supports requiring approval before running builds (to prevent malicious PRs from external contributors):

1. Enable approval requirement:
   ```bash
   ./eka-ci-server --require-approval
   ```

2. Or in systemd:
   ```ini
   [Service]
   Environment="EKA_CI_REQUIRE_APPROVAL=true"
   ```

3. Approve users via the web UI or API

### Merge Queue Support

eka-ci supports GitHub's merge queue feature:

1. Enable merge queue on your repository (Settings → General → Merge queue)
2. eka-ci will automatically receive `merge_group` events
3. Builds will run for merge queue entries

### OAuth Integration (Optional)

eka-ci also supports OAuth for web UI authentication:

```toml
[oauth]
client_id = "your-oauth-app-client-id"
client_secret = "your-oauth-app-client-secret"
redirect_url = "https://your-eka-ci-server.com/github/auth/callback"
```

**Note**: This is separate from the GitHub App and is optional.

### Migration from Environment Variables

#### From Environment Variables to Vault

**Before:**
```bash
export GITHUB_APP_ID=123456
export GITHUB_APP_PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----..."
./eka-ci-server
```

**After:**

1. Store credentials in Vault:
```bash
vault kv put secret/eka-ci/github-app \
  GITHUB_APP_ID="123456" \
  GITHUB_APP_PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----..."
```

2. Update configuration:
```toml
[[github_apps]]
id = "main"

[github_apps.credentials.vault]
address = "https://vault.example.com:8200"
secret_path = "eka-ci/github-app"
token_env = "VAULT_TOKEN"
```

3. Start server with Vault token:
```bash
export VAULT_TOKEN=your-vault-token
./eka-ci-server
```

---

## API Reference

### CredentialSource Enum

All available credential source variants:

```rust
pub enum CredentialSource {
    // Environment variables
    Env { vars: Vec<String> },

    // File-based (JSON or key=value)
    File { path: PathBuf },

    // AWS profile from ~/.aws/credentials
    AwsProfile { profile: String },

    // Cachix token (for backward compatibility)
    CachixToken { env_var: String },

    // HashiCorp Vault
    Vault {
        address: String,
        secret_path: String,
        token_env: String,
        namespace: Option<String>,
    },

    // AWS Secrets Manager
    AwsSecretsManager {
        secret_name: String,
        region: Option<String>,
    },

    // systemd credentials
    SystemdCredential { name: String },

    // Instance metadata service
    InstanceMetadata,

    // GitHub App key file
    GitHubAppKeyFile {
        app_id_env: String,
        key_file: PathBuf,
    },

    // No authentication
    None,
}
```

### GitHubAppConfig Structure

```rust
pub struct GitHubAppConfig {
    pub id: String,
    pub credentials: CredentialSource,
    pub permissions: GitHubAppPermissions,
}

pub struct GitHubAppPermissions {
    pub allow_all: bool,
    pub allowed_repos: Vec<String>,
    pub allowed_branches: Vec<String>,
}
```

---

## FAQ

**Q: Can I use the same GitHub App for multiple eka-ci servers?**
A: Not recommended. Each eka-ci instance should have its own GitHub App to avoid conflicts.

**Q: What happens if my private key is compromised?**
A: Immediately revoke it on GitHub App settings and generate a new one. Update your secret manager and restart eka-ci.

**Q: Can I use a GitHub Personal Access Token instead?**
A: No, eka-ci requires a GitHub App. Personal access tokens don't support the required webhooks and permissions model.

**Q: Do I need a separate GitHub App for each repository?**
A: No, one GitHub App can be installed on multiple repositories in the same organization.

**Q: How do I migrate from environment variables to Vault?**
A: See [Migration from Environment Variables](#migration-from-environment-variables).

**Q: Is webhook signature verification implemented?**
A: Not yet. It's mentioned in the architecture but not implemented. You should still configure a webhook secret for future use.

**Q: Which credential source should I use for production?**
A: HashiCorp Vault is recommended for best security. AWS Secrets Manager is good for AWS deployments. systemd credentials are excellent for single-server deployments with TPM2.

**Q: Can I configure multiple GitHub Apps?**
A: Yes, you can configure multiple apps in the config file, but currently only the first one will be used. Use permission controls to route different repos to different apps (with the caveat that only the first app is active).

---

## Related Documentation

- [Configuring Caches](configure-caches.md) - Similar credential source configuration for caches
- [Security Best Practices](security-best-practices.md) - General security guidelines (if exists)
- [Deployment Guide](deployment.md) - Production deployment strategies (if exists)

---

## Contributing

If you encounter issues with this setup process:

1. Check existing issues: https://github.com/ekala-project/eka-ci/issues
2. Report bugs with detailed logs and configuration (redact secrets!)
3. Contribute improvements to this documentation

---

**Last Updated:** 2024-04-10
**eka-ci Version:** Latest
