use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use aws_credential_types::provider::ProvideCredentials;
use clap::Parser;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::secret::Redacted;

mod remote_builder;
pub use remote_builder::RemoteBuilder;
use remote_builder::read_nix_machines_file;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct ConfigCli {
    /// Port for server to host http traffic
    #[arg(short, long)]
    pub port: Option<u16>,

    /// IPv4 address to bind http traffic
    #[arg(short, long)]
    pub addr: Option<Ipv4Addr>,

    /// Socket for ekaci client. Defaults to $XDG_RUNTIME_DIR/ekaci.
    #[arg(short, long)]
    pub socket: Option<PathBuf>,

    /// Path for the sqlite db path. Defaults to $XDG_DATA_HOME/ekaci/sqlite.db
    #[arg(short, long)]
    pub db_path: Option<PathBuf>,

    /// Directory for build logs. Defaults to $XDG_DATA_HOME/ekaci/build-logs
    #[arg(short, long)]
    pub logs_dir: Option<PathBuf>,

    /// Path for the configuration file. Can also be set using the $EKA_CI_CONFIG_FILE.
    /// If not provided a default path will be attempted, based on the XDG spec.
    #[arg(long)]
    pub config_file: Option<PathBuf>,

    /// Require approval before allowing jobset creation for PRs. Defaults to false.
    /// When enabled, only GitHub users in the ApprovedUsers table can trigger builds.
    #[arg(long)]
    pub require_approval: Option<bool>,

    /// Require approval before allowing jobset creation for merge queue builds. Defaults to false.
    /// When enabled, only GitHub users in the ApprovedUsers table can trigger merge queue builds.
    #[arg(long)]
    pub merge_queue_require_approval: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFile {
    web: ConfigFileWeb,
    unix: ConfigFileUnix,
    oauth: ConfigFileOAuth,
    db_path: Option<PathBuf>,
    logs_dir: Option<PathBuf>,
    require_approval: Option<bool>,
    merge_queue_require_approval: Option<bool>,
    build_no_output_timeout_seconds: Option<u64>,
    /// Absolute wall-clock cap per build (M5). Unlike the no-output
    /// timeout, this is not reset when the child emits stdout/stderr,
    /// so a derivation that prints one byte per minute for hours still
    /// terminates. Default: 14400 seconds (4 hours).
    build_max_duration_seconds: Option<u64>,
    graph_lru_capacity: Option<usize>,
    default_merge_method: Option<String>,
    #[serde(default)]
    caches: Vec<CacheConfig>,
    #[serde(default)]
    github_apps: Vec<GitHubAppConfig>,
    security: Option<SecurityConfig>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFileWeb {
    pub address: Option<Ipv4Addr>,
    pub port: Option<u16>,
    pub bundle_path: Option<PathBuf>,
    /// Explicit list of origins allowed by CORS for the HTTP API.
    ///
    /// Each entry is an exact-match scheme+host+port origin, e.g.
    /// `"https://dashboard.example.com"` or `"http://localhost:5173"`.
    /// Matching is case-sensitive and does not interpret wildcards:
    /// a literal `"*"` is rejected at load time.
    ///
    /// If unset or empty the server refuses all cross-origin requests
    /// (same-origin API only). This is intentional — historical
    /// behaviour used `CorsLayer::permissive()` which echoed any
    /// `Origin` back as `Access-Control-Allow-Origin` and is safe
    /// today only because auth uses `Authorization: Bearer` (a
    /// non-simple request header). The strict default protects any
    /// future cookie or same-site endpoint from ambient-authority
    /// CSRF.
    #[serde(default)]
    pub allowed_origins: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFileUnix {
    pub socket_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFileOAuth {
    pub client_id: Option<String>,
    // M2: wrap secrets so `ConfigFile`'s derived `Debug` cannot leak
    // them via log formatting. Serde is transparent, so TOML/env
    // round-trip as plain strings.
    pub client_secret: Option<Redacted<String>>,
    pub redirect_url: Option<String>,
    pub jwt_secret: Option<Redacted<String>>,
}

/// Cache registry configuration - defines available caches server-side
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CacheConfig {
    /// Cache ID (referenced from repository .eka-ci/config.json)
    pub id: String,
    /// Type of cache (nix-copy, cachix, attic)
    pub cache_type: CacheType,
    /// Destination URL or identifier (e.g., s3://bucket/path, cachix-cache-name)
    pub destination: String,
    /// Credential source for authentication
    pub credentials: CredentialSource,
    /// Permissions controlling which repos/branches can use this cache
    #[serde(default)]
    pub permissions: CachePermissions,
}

/// Types of caches supported by the system
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheType {
    /// Uses `nix copy` to push to S3 or HTTP caches
    NixCopy,
    /// Uses Cachix for pushing
    Cachix,
    /// Uses Attic for pushing
    Attic,
}

/// Where to retrieve credentials from
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialSource {
    /// Read from environment variable (e.g., AWS_ACCESS_KEY_ID)
    Env {
        /// Environment variable names to read
        vars: Vec<String>,
    },
    /// Read from a file path
    File {
        /// Path to credential file
        path: PathBuf,
    },
    /// Use AWS profile from ~/.aws/credentials
    AwsProfile {
        /// Profile name
        profile: String,
    },
    /// Use Cachix auth token from environment or config
    CachixToken {
        /// Environment variable containing token
        env_var: String,
    },
    /// Retrieve from HashiCorp Vault
    Vault {
        /// Vault address (e.g., https://vault.example.com:8200)
        address: String,
        /// Secret path (e.g., secret/data/eka-ci/s3-cache)
        secret_path: String,
        /// Optional: Vault token from environment variable (default: VAULT_TOKEN)
        #[serde(default = "default_vault_token_var")]
        token_env: String,
        /// Optional: Vault namespace
        #[serde(default)]
        namespace: Option<String>,
    },
    /// Retrieve from AWS Secrets Manager
    AwsSecretsManager {
        /// Secret name or ARN
        secret_name: String,
        /// Optional: AWS region (defaults to AWS_REGION env var)
        #[serde(default)]
        region: Option<String>,
    },
    /// Retrieve from systemd credentials (systemd-creds)
    SystemdCredential {
        /// Credential name
        name: String,
    },
    /// Use instance metadata service (EC2/GCP/Azure IMDS)
    /// Works without explicit credentials for cloud VMs with IAM roles
    InstanceMetadata,
    /// GitHub App private key from file (app_id from env var)
    GitHubAppKeyFile {
        /// Environment variable containing the app ID
        app_id_env: String,
        /// Path to PEM-encoded private key file
        key_file: PathBuf,
    },
    /// No authentication required
    None,
}

fn default_vault_token_var() -> String {
    "VAULT_TOKEN".to_string()
}

impl CredentialSource {
    /// Load credentials from the configured source
    /// Returns a HashMap of environment variable key-value pairs
    pub async fn load(&self) -> anyhow::Result<HashMap<String, String>> {
        match self {
            CredentialSource::Env { vars } => Self::load_from_env(vars),
            CredentialSource::File { path } => Self::load_from_file(path).await,
            CredentialSource::AwsProfile { profile } => Self::load_aws_profile(profile).await,
            CredentialSource::CachixToken { env_var } => Self::load_cachix_token(env_var),
            CredentialSource::Vault {
                address,
                secret_path,
                token_env,
                namespace,
            } => Self::load_from_vault(address, secret_path, token_env, namespace.as_deref()).await,
            CredentialSource::AwsSecretsManager {
                secret_name,
                region,
            } => Self::load_aws_secrets_manager(secret_name, region.as_deref()).await,
            CredentialSource::SystemdCredential { name } => {
                Self::load_systemd_credential(name).await
            },
            CredentialSource::InstanceMetadata => Self::load_instance_metadata().await,
            CredentialSource::GitHubAppKeyFile {
                app_id_env,
                key_file,
            } => Self::load_github_app_key_file(app_id_env, key_file).await,
            CredentialSource::None => {
                debug!("No credentials required");
                Ok(HashMap::new())
            },
        }
    }

    fn load_from_env(vars: &[String]) -> anyhow::Result<HashMap<String, String>> {
        // M2: `vars` contains only environment variable *names*, not
        // their values — but even the names (e.g. `AWS_SECRET_ACCESS_KEY`)
        // hint at what is in flight. Keep this at `trace!` so it is
        // opt-in for debugging and does not surface under the default
        // log filter.
        tracing::trace!("Loading credentials from environment variables: {:?}", vars);
        let mut result = HashMap::new();
        for var in vars {
            let value = std::env::var(var)
                .with_context(|| format!("Environment variable {} not found", var))?;
            result.insert(var.clone(), value);
        }
        Ok(result)
    }

    async fn load_from_file(path: &PathBuf) -> anyhow::Result<HashMap<String, String>> {
        debug!("Loading credentials from file: {}", path.display());
        let contents = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read credential file: {}", path.display()))?;

        // Parse as JSON or env-style key=value pairs
        if let Ok(json_creds) = serde_json::from_str::<HashMap<String, String>>(&contents) {
            return Ok(json_creds);
        }

        // Try parsing as KEY=VALUE lines
        let mut result = HashMap::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                result.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        if result.is_empty() {
            bail!("Failed to parse credentials from file: {}", path.display());
        }
        Ok(result)
    }

    async fn load_aws_profile(profile: &str) -> anyhow::Result<HashMap<String, String>> {
        debug!("Loading AWS credentials from profile: {}", profile);
        // AWS credentials are typically in ~/.aws/credentials
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        let creds_path = PathBuf::from(home).join(".aws/credentials");

        let contents = tokio::fs::read_to_string(&creds_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to read AWS credentials file: {}",
                    creds_path.display()
                )
            })?;

        let mut in_profile = false;
        let mut result = HashMap::new();

        for line in contents.lines() {
            let line = line.trim();

            // Check for profile header
            if line.starts_with('[') && line.ends_with(']') {
                let profile_name = &line[1..line.len() - 1];
                in_profile = profile_name == profile;
                continue;
            }

            if in_profile {
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    // Map AWS credential file keys to environment variable names
                    let env_key = match key {
                        "aws_access_key_id" => "AWS_ACCESS_KEY_ID",
                        "aws_secret_access_key" => "AWS_SECRET_ACCESS_KEY",
                        "aws_session_token" => "AWS_SESSION_TOKEN",
                        _ => key,
                    };
                    result.insert(env_key.to_string(), value.to_string());
                }
            }
        }

        if result.is_empty() {
            bail!("AWS profile '{}' not found or empty", profile);
        }

        Ok(result)
    }

    fn load_cachix_token(env_var: &str) -> anyhow::Result<HashMap<String, String>> {
        debug!("Loading Cachix token from environment: {}", env_var);
        let token = std::env::var(env_var)
            .with_context(|| format!("Cachix token environment variable {} not found", env_var))?;

        let mut result = HashMap::new();
        result.insert("CACHIX_AUTH_TOKEN".to_string(), token);
        Ok(result)
    }

    async fn load_from_vault(
        address: &str,
        secret_path: &str,
        token_env: &str,
        namespace: Option<&str>,
    ) -> anyhow::Result<HashMap<String, String>> {
        debug!(
            "Loading credentials from Vault: {} at {}",
            address, secret_path
        );

        let token = std::env::var(token_env)
            .with_context(|| format!("Vault token environment variable {} not found", token_env))?;

        // Create Vault client settings builder
        let mut settings_builder = vaultrs::client::VaultClientSettingsBuilder::default();
        settings_builder.address(address).token(&token);

        // Set namespace if provided (through settings builder)
        if let Some(ns) = namespace {
            settings_builder.namespace(Some(ns.to_string()));
        }

        let client = vaultrs::client::VaultClient::new(
            settings_builder
                .build()
                .context("Failed to build Vault client settings")?,
        )
        .context("Failed to create Vault client")?;

        // Read secret from Vault KV v2
        let secret: HashMap<String, String> = vaultrs::kv2::read(&client, "secret", secret_path)
            .await
            .with_context(|| format!("Failed to read secret from Vault path: {}", secret_path))?;

        Ok(secret)
    }

    async fn load_aws_secrets_manager(
        secret_name: &str,
        region: Option<&str>,
    ) -> anyhow::Result<HashMap<String, String>> {
        debug!(
            "Loading credentials from AWS Secrets Manager: {}",
            secret_name
        );

        // Load AWS config
        let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

        if let Some(r) = region {
            config_loader = config_loader.region(aws_config::Region::new(r.to_string()));
        }

        let config = config_loader.load().await;
        let client = aws_sdk_secretsmanager::Client::new(&config);

        let response = client
            .get_secret_value()
            .secret_id(secret_name)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Failed to retrieve secret from AWS Secrets Manager: {}",
                    secret_name
                )
            })?;

        let secret_string = response
            .secret_string()
            .ok_or_else(|| anyhow!("Secret value is not a string"))?;

        // Parse as JSON
        let credentials: HashMap<String, String> = serde_json::from_str(secret_string)
            .context("Failed to parse AWS Secrets Manager secret as JSON")?;

        Ok(credentials)
    }

    async fn load_systemd_credential(name: &str) -> anyhow::Result<HashMap<String, String>> {
        debug!("Loading credentials from systemd credential: {}", name);

        // Try to read from systemd credentials directory
        // Systemd stores credentials in $CREDENTIALS_DIRECTORY or
        // /run/credentials/<service>
        let cred_dir = std::env::var("CREDENTIALS_DIRECTORY")
            .unwrap_or_else(|_| "/run/credentials".to_string());

        let cred_path = PathBuf::from(cred_dir).join(name);

        let contents = tokio::fs::read_to_string(&cred_path)
            .await
            .with_context(|| {
                format!("Failed to read systemd credential: {}", cred_path.display())
            })?;

        // Parse as JSON or key=value
        if let Ok(json_creds) = serde_json::from_str::<HashMap<String, String>>(&contents) {
            Ok(json_creds)
        } else {
            // Treat as single value credential
            let mut result = HashMap::new();
            result.insert(name.to_string(), contents.trim().to_string());
            Ok(result)
        }
    }

    async fn load_instance_metadata() -> anyhow::Result<HashMap<String, String>> {
        debug!("Loading credentials from instance metadata service");

        // For AWS EC2, this will use the instance profile
        // The AWS SDK automatically uses IMDS when available
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;

        // Verify that we have credentials
        let credentials = config
            .credentials_provider()
            .ok_or_else(|| anyhow!("No credentials available from instance metadata"))?
            .provide_credentials()
            .await
            .context("Failed to load credentials from instance metadata")?;

        let mut result = HashMap::new();
        result.insert(
            "AWS_ACCESS_KEY_ID".to_string(),
            credentials.access_key_id().to_string(),
        );
        result.insert(
            "AWS_SECRET_ACCESS_KEY".to_string(),
            credentials.secret_access_key().to_string(),
        );

        if let Some(token) = credentials.session_token() {
            result.insert("AWS_SESSION_TOKEN".to_string(), token.to_string());
        }

        Ok(result)
    }

    async fn load_github_app_key_file(
        app_id_env: &str,
        key_file: &PathBuf,
    ) -> anyhow::Result<HashMap<String, String>> {
        debug!(
            "Loading GitHub App credentials from file: {}",
            key_file.display()
        );

        let app_id = std::env::var(app_id_env).with_context(|| {
            format!(
                "GitHub App ID environment variable {} not found",
                app_id_env
            )
        })?;

        let private_key = tokio::fs::read_to_string(key_file).await.with_context(|| {
            format!(
                "Failed to read GitHub App private key file: {}",
                key_file.display()
            )
        })?;

        let mut result = HashMap::new();
        result.insert("GITHUB_APP_ID".to_string(), app_id);
        result.insert("GITHUB_APP_PRIVATE_KEY".to_string(), private_key);

        Ok(result)
    }
}

/// Controls which repos/branches can use a cache
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CachePermissions {
    /// Allow all repositories (default: true)
    #[serde(default = "default_true")]
    pub allow_all: bool,
    /// Specific repositories allowed (owner/repo format)
    #[serde(default)]
    pub allowed_repos: Vec<String>,
    /// Branch patterns allowed (glob patterns)
    #[serde(default)]
    pub allowed_branches: Vec<String>,
}

/// GitHub App configuration - defines available GitHub Apps server-side
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GitHubAppConfig {
    /// GitHub App ID (referenced from repository .eka-ci/config.json)
    pub id: String,
    /// Credential source for GitHub App authentication
    pub credentials: CredentialSource,
    /// Permissions controlling which repos/branches can use this GitHub App
    #[serde(default)]
    pub permissions: GitHubAppPermissions,
}

/// Controls which repos/branches can use a GitHub App
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GitHubAppPermissions {
    /// Allow all repositories (default: true)
    #[serde(default = "default_true")]
    pub allow_all: bool,
    /// Specific repositories allowed (owner/repo format)
    #[serde(default)]
    pub allowed_repos: Vec<String>,
    /// Branch patterns allowed (glob patterns)
    #[serde(default)]
    pub allowed_branches: Vec<String>,
}

/// Security configuration for hook execution
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SecurityConfig {
    /// Maximum hook execution time in seconds (default: 300)
    #[serde(default = "default_hook_timeout")]
    pub max_hook_timeout_seconds: u64,
    /// Enable audit logging of hook executions (default: true)
    #[serde(default = "default_true")]
    pub audit_hooks: bool,
    /// GitHub webhook secret for signature verification.
    ///
    /// Startup fails if this is unset unless `allow_insecure_webhooks`
    /// is explicitly enabled (development / local-test only).
    ///
    /// M2: wrapped in `Redacted<_>` so this value cannot leak via
    /// `Debug` / log formatting of the surrounding config types.
    /// Serde is transparent, so TOML/env still parse as plain strings.
    #[serde(default)]
    pub webhook_secret: Option<Redacted<String>>,
    /// Escape hatch: allow the server to start without a webhook
    /// secret. Intended for local development and tests only.
    ///
    /// When enabled **and** `webhook_secret` is unset, the webhook
    /// handler accepts payloads without signature verification (the
    /// pre-H1 behaviour). A startup-time warning is logged on every
    /// boot so operators cannot forget the flag is on. Setting this
    /// to `true` in a production-exposed deployment is a
    /// security-critical mistake.
    #[serde(default)]
    pub allow_insecure_webhooks: bool,
    /// M6 escape hatch: permit `[[caches]].destination` values whose
    /// host resolves to a private, loopback, or link-local address
    /// (e.g. `http://127.0.0.1:5000`, `http://10.0.0.5`, `ssh://[::1]`).
    ///
    /// By default (`false`) such destinations are rejected at startup
    /// to prevent an operator or malicious PR author from coercing the
    /// build host into issuing requests to internal services or cloud
    /// metadata endpoints (SSRF). Enable only for local development
    /// or isolated test deployments where the private host is known
    /// to be safe.
    #[serde(default)]
    pub allow_private_cache_hosts: bool,
}

fn default_hook_timeout() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

// Timeout clamp bounds. Rejects obvious misconfiguration (0, u64::MAX)
// while leaving legitimate operational choices untouched.
const BUILD_NO_OUTPUT_TIMEOUT_MIN_S: u64 = 30;
const BUILD_NO_OUTPUT_TIMEOUT_MAX_S: u64 = 24 * 60 * 60;
const BUILD_MAX_DURATION_MIN_S: u64 = 60;
const BUILD_MAX_DURATION_MAX_S: u64 = 7 * 24 * 60 * 60;
const HOOK_TIMEOUT_MIN_S: u64 = 1;
const HOOK_TIMEOUT_MAX_S: u64 = 24 * 60 * 60;

/// Clamp a timeout (seconds) into `[min, max]`, warning if clamped.
fn clamp_timeout_seconds(field: &'static str, value: u64, min: u64, max: u64) -> u64 {
    debug_assert!(
        min < max,
        "clamp_timeout_seconds: min ({min}) >= max ({max})"
    );
    if value < min {
        warn!(
            field,
            requested = value,
            clamped_to = min,
            "timeout below minimum; clamping up"
        );
        return min;
    }
    if value > max {
        warn!(
            field,
            requested = value,
            clamped_to = max,
            "timeout above maximum; clamping down"
        );
        return max;
    }
    value
}

/// Validate a CORS allow-list entry.
///
/// A valid entry is a fully-qualified origin — scheme + authority
/// with no path, query, or fragment (e.g. `"https://app.example.com"`,
/// `"http://localhost:5173"`). This deliberately rejects `"*"` so
/// the allow-list cannot be turned into an allow-all by accident.
pub(crate) fn validate_cors_origin(origin: &str) -> anyhow::Result<()> {
    if origin.is_empty() {
        bail!("origin is empty");
    }
    if origin == "*" {
        bail!("wildcard origin `*` is not accepted; list each origin explicitly");
    }
    let url = url::Url::parse(origin).with_context(|| format!("cannot parse {origin:?}"))?;
    match url.scheme() {
        "http" | "https" => {},
        other => bail!("origin scheme {other:?} is not http or https"),
    }
    if url.host_str().is_none() {
        bail!("origin {origin:?} has no host");
    }
    // `url::Url::parse("https://a.b/")` returns `path = "/"`. Accept
    // both `""` and `"/"` to be forgiving, but reject anything else.
    if !matches!(url.path(), "" | "/") {
        bail!("origin {origin:?} must not include a path");
    }
    if url.query().is_some() {
        bail!("origin {origin:?} must not include a query string");
    }
    if url.fragment().is_some() {
        bail!("origin {origin:?} must not include a fragment");
    }
    if url.username() != "" || url.password().is_some() {
        bail!("origin {origin:?} must not include userinfo");
    }
    Ok(())
}

/// M6: validate a `[[caches]].destination` value.
///
/// The `destination` field is passed directly as an argv element to
/// `nix copy --to <dest>`, `cachix push <dest>`, or `attic push <dest>`
/// (see `scheduler::recorder::build_cache_push_hook`). The sub-command
/// in turn issues network requests to the given endpoint, so an
/// unvalidated destination is an SSRF primitive: an operator-authored
/// or config-reload-authored value such as
/// `http://169.254.169.254/...` would cause the build host to hit the
/// EC2/GCP metadata endpoint on every build.
///
/// This validator enforces:
///   * If the value looks like a URL (contains `"://"`), the scheme must be in a small allow-list
///     and the host must not resolve to a private/loopback/link-local address unless
///     `allow_private_hosts == true`.
///   * Bare identifiers (no `"://"`) are only accepted for `CacheType::Cachix` and must match a
///     conservative `[A-Za-z0-9._-]+` pattern so they cannot smuggle argv separators or shell
///     metacharacters downstream.
///
/// Returns `Err` on rejection so startup aborts with a clear message
/// rather than silently deferring the failure to the first push.
pub(crate) fn validate_cache_destination(
    destination: &str,
    cache_type: &CacheType,
    allow_private_hosts: bool,
) -> anyhow::Result<()> {
    if destination.is_empty() {
        bail!("cache destination is empty");
    }

    // Bare identifier path (Cachix cache name). `nix copy` and `attic`
    // both require a scheme, so anything without `"://"` is only
    // meaningful for Cachix.
    if !destination.contains("://") {
        if !matches!(cache_type, CacheType::Cachix) {
            bail!(
                "cache destination {destination:?} has no scheme; only `cachix` caches may use a \
                 bare identifier (e.g. `mycache`). Use e.g. `s3://…`, `https://…`, or `ssh://…` \
                 for `nix-copy` / `attic`."
            );
        }
        // Conservative pattern: ASCII alnum plus `.`, `_`, `-`. This is
        // a superset of Cachix cache names but excludes whitespace,
        // shell metacharacters, and argv separators.
        let ok = destination
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok {
            bail!(
                "cachix destination {destination:?} contains disallowed characters; expected \
                 [A-Za-z0-9._-]+"
            );
        }
        return Ok(());
    }

    // URL path — scheme + host + private-address checks.
    let url = url::Url::parse(destination)
        .with_context(|| format!("cache destination {destination:?} is not a valid URL"))?;

    // Allow-list schemes. Everything outside this set is rejected even
    // if `url` would happily parse it (e.g. `javascript:`, `file:`,
    // `data:`) — cache pushing has no use for them and they are a
    // common SSRF / LFI vector if the downstream tool happens to
    // honour them.
    match url.scheme() {
        // Recognised by `nix copy` / `cachix` / `attic`.
        "s3" | "https" | "http" | "ssh" | "ssh-ng" => {},
        other => bail!(
            "cache destination {destination:?} uses disallowed scheme {other:?}; allowed: s3, \
             https, http, ssh, ssh-ng"
        ),
    }

    // `url::Url` requires a host for `http`/`https`/`ssh`, but `s3://`
    // URLs can legitimately encode the bucket as the host. Either
    // way, absence of a host is a hard error — we cannot apply the
    // private-address check to it. `url` sometimes returns
    // `Some(Domain(""))` for inputs like `https:///path`, so guard
    // against the empty-string case too.
    let host = url
        .host()
        .ok_or_else(|| anyhow!("cache destination {destination:?} has no host component"))?;
    if let url::Host::Domain(d) = &host {
        if d.is_empty() {
            bail!("cache destination {destination:?} has an empty host component");
        }
    }

    if !allow_private_hosts && host_is_private(&host) {
        bail!(
            "cache destination {destination:?} resolves to a private / loopback / link-local \
             host; set `security.allow_private_cache_hosts = true` (or \
             `EKACI_ALLOW_PRIVATE_CACHE_HOSTS=1`) to override. This check prevents accidental \
             SSRF into internal services or cloud-metadata endpoints."
        );
    }

    Ok(())
}

/// M6 helper: is the given `url::Host` "private" in the SSRF sense?
///
/// Covers RFC 1918 IPv4, loopback (127.0.0.0/8, ::1), link-local
/// (169.254.0.0/16 incl. cloud metadata, fe80::/10), unique-local IPv6
/// (fc00::/7), unspecified (0.0.0.0, ::), and a small set of hostnames
/// that commonly point at localhost.
fn host_is_private(host: &url::Host<&str>) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    match host {
        url::Host::Ipv4(ip) => ipv4_is_private(ip),
        url::Host::Ipv6(ip) => ipv6_is_private(ip),
        url::Host::Domain(name) => {
            let lower = name.to_ascii_lowercase();
            // Exact-match hostnames commonly resolving to the loopback
            // interface or cloud-metadata service. This list is
            // intentionally conservative — a determined operator can
            // still register e.g. `internal.example.com -> 127.0.0.1`,
            // but those cases require `allow_private_cache_hosts`.
            if matches!(
                lower.as_str(),
                "localhost"
                    | "localhost.localdomain"
                    | "ip6-localhost"
                    | "ip6-loopback"
                    | "metadata.google.internal"
                    | "metadata.goog"
            ) {
                return true;
            }
            // Reject suffix matches for common loopback-ish TLDs used
            // by local resolvers / dev setups.
            if lower.ends_with(".localhost") || lower.ends_with(".local") {
                return true;
            }
            // Some operators configure `[[caches]].destination` with a
            // literal numeric host that `url` parsed as a `Domain`
            // when using IPv6 brackets or unusual formats. Attempt a
            // best-effort parse.
            if let Ok(v4) = lower.parse::<Ipv4Addr>() {
                return ipv4_is_private(&v4);
            }
            if let Ok(v6) = lower.parse::<Ipv6Addr>() {
                return ipv6_is_private(&v6);
            }
            false
        },
    }
}

fn ipv4_is_private(ip: &std::net::Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        // 100.64.0.0/10 — CGNAT; treat as private to be conservative.
        || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 0x40)
}

fn ipv6_is_private(ip: &std::net::Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let segments = ip.segments();
    // fe80::/10 — link-local.
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // fc00::/7 — unique local addresses (ULA).
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // IPv4-mapped (::ffff:0:0/96) — extract and re-check against v4.
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        let octets = ip.octets();
        let v4 = std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
        return ipv4_is_private(&v4);
    }
    false
}

#[derive(Deserialize, Debug)]
struct ConfigEnv {
    #[serde(rename = "eka_ci_config_file")]
    pub config_file: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Config {
    pub web: ConfigWeb,
    pub unix: ConfigUnix,
    pub oauth: ConfigOAuth,
    pub db_path: PathBuf,
    pub logs_dir: PathBuf,
    #[allow(dead_code)]
    pub remote_builders: Vec<RemoteBuilder>,
    pub require_approval: bool,
    pub merge_queue_require_approval: bool,
    pub build_no_output_timeout_seconds: u64,
    /// Absolute per-build wall-clock cap (M5). Default 14400 s (4 h);
    /// override with `EKA_CI_BUILD_MAX_DURATION_SECONDS` or the
    /// `build_max_duration_seconds` field of `ekaci.toml`.
    pub build_max_duration_seconds: u64,
    /// Maximum number of nodes in the LRU cache (default: 100,000)
    pub graph_lru_capacity: usize,
    /// Default merge method for auto-merge (merge, squash, rebase)
    pub default_merge_method: String,
    /// Cache registry - maps cache IDs to configurations
    pub caches: HashMap<String, CacheConfig>,
    /// GitHub App registry - maps app IDs to configurations
    pub github_apps: HashMap<String, GitHubAppConfig>,
    /// Security settings for hook execution
    pub security: SecurityConfig,
}

#[derive(Debug)]
pub struct ConfigOAuth {
    pub client_id: String,
    // M2: wrap secrets so `Debug` / structured log formatters can
    // never leak them. Read access must go through
    // [`Redacted::expose`].
    pub client_secret: Redacted<String>,
    pub redirect_url: String,
    pub jwt_secret: Redacted<String>,
}

#[derive(Debug)]
pub struct ConfigWeb {
    pub address: SocketAddrV4,
    /// Validated CORS allow-list. See `ConfigFileWeb::allowed_origins`
    /// for semantics. Empty means "refuse all cross-origin requests".
    pub allowed_origins: Vec<String>,
}

#[derive(Debug)]
pub struct ConfigUnix {
    pub socket_path: PathBuf,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let args = ConfigCli::parse();
        let dirs = xdg::BaseDirectories::with_prefix("ekaci")?;
        let env = envy::from_env::<ConfigEnv>()?;

        let config_path = args
            .config_file
            .or(env.config_file)
            .unwrap_or(dirs.get_config_file("ekaci.toml"));

        info!("Loading configuration file from {}", config_path.display());

        let file = Figment::from(Serialized::defaults(ConfigFile::default()))
            .merge(Toml::file(config_path))
            .merge(Env::prefixed("EKA_CI_").split("__"))
            .extract::<ConfigFile>()
            .context("failed to parse config file")?;

        let remote_builders = read_nix_machines_file();

        // OAuth configuration from environment or file
        let oauth_client_id = std::env::var("GITHUB_OAUTH_CLIENT_ID")
            .ok()
            .or(file.oauth.client_id)
            .unwrap_or_else(|| {
                info!("GITHUB_OAUTH_CLIENT_ID not set, OAuth will not work");
                String::new()
            });

        let oauth_client_secret = std::env::var("GITHUB_OAUTH_CLIENT_SECRET")
            .ok()
            .map(Redacted::new)
            .or(file.oauth.client_secret)
            .unwrap_or_else(|| {
                info!("GITHUB_OAUTH_CLIENT_SECRET not set, OAuth will not work");
                Redacted::new(String::new())
            });

        // Determine the actual bind address for the OAuth callback URL
        let bind_addr = args
            .addr
            .or(file.web.address)
            .unwrap_or_else(|| Ipv4Addr::new(127, 0, 0, 1));
        let bind_port = args.port.or(file.web.port).unwrap_or(3030);

        let oauth_redirect_url = std::env::var("GITHUB_OAUTH_REDIRECT_URL")
            .ok()
            .or(file.oauth.redirect_url)
            .unwrap_or_else(|| {
                // Default to the actual server address and port
                format!("http://{}:{}/github/auth/callback", bind_addr, bind_port)
            });

        let jwt_secret = std::env::var("JWT_SECRET")
            .ok()
            .map(Redacted::new)
            .or(file.oauth.jwt_secret)
            .unwrap_or_else(|| {
                // No configured secret: generate a cryptographically random
                // 256-bit secret via the OS CSPRNG. This is intentionally
                // not derived from time, PID, or any other predictable
                // source, which closes the old `insecure-secret-{timestamp}`
                // brute-force path. Issued tokens will not survive a server
                // restart, which is the correct failure mode — operators
                // are expected to configure JWT_SECRET for production.
                let mut bytes = [0u8; 32];
                rand::fill(&mut bytes);
                tracing::warn!(
                    event = "jwt_secret_generated_ephemeral",
                    "JWT_SECRET not set; generated an ephemeral 256-bit secret. Sessions will NOT \
                     survive a server restart. Configure JWT_SECRET (or oauth.jwt_secret) for \
                     production."
                );
                Redacted::new(hex::encode(bytes))
            });

        // Get security config or use defaults (needed early for the
        // M6 cache-destination validator below).
        let mut security = file.security.unwrap_or_else(|| SecurityConfig {
            max_hook_timeout_seconds: default_hook_timeout(),
            audit_hooks: true,
            webhook_secret: None,
            allow_insecure_webhooks: false,
            allow_private_cache_hosts: false,
        });

        // Env override for the M6 escape hatch. Accepts the usual
        // boolean spellings so shell `=1` / `=true` both work.
        if let Ok(raw) = std::env::var("EKACI_ALLOW_PRIVATE_CACHE_HOSTS") {
            let normalized = raw.trim().to_ascii_lowercase();
            security.allow_private_cache_hosts =
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on");
        }

        security.max_hook_timeout_seconds = clamp_timeout_seconds(
            "security.max_hook_timeout_seconds",
            security.max_hook_timeout_seconds,
            HOOK_TIMEOUT_MIN_S,
            HOOK_TIMEOUT_MAX_S,
        );

        // M6: validate every `[[caches]].destination` before building
        // the registry. An invalid entry aborts startup so operators
        // see the problem immediately, the same pattern used for CORS
        // origins above.
        let mut caches: HashMap<String, CacheConfig> = HashMap::with_capacity(file.caches.len());
        for cache in file.caches {
            validate_cache_destination(
                &cache.destination,
                &cache.cache_type,
                security.allow_private_cache_hosts,
            )
            .with_context(|| {
                format!(
                    "invalid [[caches]] entry id={:?} destination={:?}",
                    cache.id, cache.destination
                )
            })?;
            if caches.insert(cache.id.clone(), cache).is_some() {
                // Duplicate IDs silently clobbered before M6; surface
                // them now because later lookups rely on a single
                // canonical config per ID.
                // (Intentionally not a hard error to preserve
                //  backwards compat with existing deployments that
                //  may accidentally duplicate; emit a warning only.)
                tracing::warn!(
                    event = "cache_config_duplicate_id",
                    "duplicate [[caches]] id; last entry wins"
                );
            }
        }

        // Build GitHub App registry as a HashMap
        let github_apps = file
            .github_apps
            .into_iter()
            .map(|app| (app.id.clone(), app))
            .collect::<HashMap<String, GitHubAppConfig>>();

        // Allow webhook_secret to be overridden by environment variable
        if let Ok(secret) = std::env::var("GITHUB_WEBHOOK_SECRET") {
            security.webhook_secret = Some(Redacted::new(secret));
        }

        // Env override for the insecure-webhooks escape hatch. Accepts
        // typical boolean spellings so shell `=1` and `=true` both work.
        if let Ok(raw) = std::env::var("EKACI_ALLOW_INSECURE_WEBHOOKS") {
            let normalized = raw.trim().to_ascii_lowercase();
            security.allow_insecure_webhooks =
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on");
        }

        // M1: resolve the CORS allow-list. Precedence (highest wins):
        //   1. EKACI_WEB_ALLOWED_ORIGINS env var (comma-separated)
        //   2. [web].allowed_origins in the config file
        //   3. empty list (strict default — same-origin only)
        //
        // Each entry is individually validated to be (a) non-empty,
        // (b) not `*` (no wildcard allow-all), and (c) a parseable
        // `http://` or `https://` origin with no path/query/fragment.
        // Invalid entries abort startup so the operator sees the
        // problem immediately instead of silently losing cross-origin
        // access at runtime.
        let mut allowed_origins: Vec<String> = match std::env::var("EKACI_WEB_ALLOWED_ORIGINS") {
            Ok(raw) => raw
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            Err(_) => file.web.allowed_origins.unwrap_or_default(),
        };
        for origin in &allowed_origins {
            validate_cors_origin(origin)
                .with_context(|| format!("invalid web.allowed_origins entry {origin:?}"))?;
        }
        // Dedupe while preserving order — it's nicer in logs and
        // avoids accidentally emitting duplicate header values.
        {
            let mut seen = HashSet::new();
            allowed_origins.retain(|o| seen.insert(o.clone()));
        }
        if allowed_origins.is_empty() {
            tracing::info!(
                event = "cors_allowlist_empty",
                "No CORS origins configured; cross-origin API requests will be rejected. Set \
                 [web].allowed_origins or EKACI_WEB_ALLOWED_ORIGINS to permit specific origins."
            );
        } else {
            tracing::info!(
                event = "cors_allowlist_configured",
                count = allowed_origins.len(),
                origins = ?allowed_origins,
                "CORS allow-list active"
            );
        }

        // H1: refuse to start without a webhook secret unless the
        // operator has explicitly opted into the insecure path. This
        // closes the old silent-accept-all-webhooks behaviour.
        if security.webhook_secret.is_none() {
            if security.allow_insecure_webhooks {
                tracing::warn!(
                    event = "webhook_secret_missing_insecure_allowed",
                    "GITHUB_WEBHOOK_SECRET is unset and allow_insecure_webhooks=true. Incoming \
                     webhooks will be accepted WITHOUT signature verification. This is safe only \
                     for local development."
                );
            } else {
                anyhow::bail!(
                    "GitHub webhook secret is not configured. Set GITHUB_WEBHOOK_SECRET (or \
                     security.webhook_secret in the config file). For local development only you \
                     may set EKACI_ALLOW_INSECURE_WEBHOOKS=1 (or \
                     security.allow_insecure_webhooks=true) to bypass this check."
                );
            }
        }

        Ok(Config {
            web: ConfigWeb {
                address: SocketAddrV4::new(bind_addr, bind_port),
                allowed_origins,
            },
            unix: ConfigUnix {
                socket_path: match args.socket.or(file.unix.socket_path) {
                    Some(p) => p,
                    None => dirs.get_runtime_file("ekaci.socket")?,
                },
            },
            oauth: ConfigOAuth {
                client_id: oauth_client_id,
                client_secret: oauth_client_secret,
                redirect_url: oauth_redirect_url,
                jwt_secret,
            },
            db_path: args
                .db_path
                .or(file.db_path)
                .unwrap_or_else(|| dirs.get_data_file("sqlite.db")),
            logs_dir: args
                .logs_dir
                .or(file.logs_dir)
                .unwrap_or_else(|| dirs.get_data_file("build-logs")),
            remote_builders,
            require_approval: args
                .require_approval
                .or(file.require_approval)
                .unwrap_or(false),
            merge_queue_require_approval: args
                .merge_queue_require_approval
                .or(file.merge_queue_require_approval)
                .unwrap_or(false),
            build_no_output_timeout_seconds: clamp_timeout_seconds(
                "build_no_output_timeout_seconds",
                file.build_no_output_timeout_seconds.unwrap_or(1200),
                BUILD_NO_OUTPUT_TIMEOUT_MIN_S,
                BUILD_NO_OUTPUT_TIMEOUT_MAX_S,
            ),
            build_max_duration_seconds: clamp_timeout_seconds(
                "build_max_duration_seconds",
                file.build_max_duration_seconds.unwrap_or(14_400),
                BUILD_MAX_DURATION_MIN_S,
                BUILD_MAX_DURATION_MAX_S,
            ),
            graph_lru_capacity: file.graph_lru_capacity.unwrap_or(100_000),
            default_merge_method: file
                .default_merge_method
                .unwrap_or_else(|| "squash".to_string()),
            caches,
            github_apps,
            security,
        })
    }

    /// Ensure the logs directory exists, creating it if necessary
    pub fn ensure_logs_dir(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.logs_dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod redaction_tests {
    //! M2 regression tests: secrets embedded in config types must not
    //! appear in `Debug` / `Display` output, even when wrapped in
    //! outer structs with derived `Debug`.
    //!
    //! These tests construct config structs directly rather than
    //! driving `from_env` because `from_env` has many side effects
    //! (file I/O, env vars, RNG seeding) that aren't relevant to
    //! verifying the redaction invariant.
    use super::*;

    const SECRET_NEEDLES: &[&str] = &[
        "oauth-client-secret-needle",
        "jwt-secret-needle",
        "webhook-secret-needle",
    ];

    fn assert_no_secret(haystack: &str, context: &str) {
        for needle in SECRET_NEEDLES {
            assert!(
                !haystack.contains(needle),
                "secret {needle:?} leaked in {context}:\n{haystack}"
            );
        }
        assert!(
            haystack.contains("[REDACTED]"),
            "no redaction marker found in {context}:\n{haystack}"
        );
    }

    #[test]
    fn config_oauth_debug_redacts_all_secrets() {
        let oauth = ConfigOAuth {
            client_id: "client-id".to_string(),
            client_secret: Redacted::new("oauth-client-secret-needle".to_string()),
            redirect_url: "http://localhost/callback".to_string(),
            jwt_secret: Redacted::new("jwt-secret-needle".to_string()),
        };
        assert_no_secret(&format!("{oauth:?}"), "ConfigOAuth {:?}");
        assert_no_secret(&format!("{oauth:#?}"), "ConfigOAuth {:#?}");
    }

    #[test]
    fn security_config_debug_redacts_webhook_secret() {
        let sec = SecurityConfig {
            max_hook_timeout_seconds: 300,
            audit_hooks: true,
            webhook_secret: Some(Redacted::new("webhook-secret-needle".to_string())),
            allow_insecure_webhooks: false,
            allow_private_cache_hosts: false,
        };
        assert_no_secret(&format!("{sec:?}"), "SecurityConfig {:?}");
        assert_no_secret(&format!("{sec:#?}"), "SecurityConfig {:#?}");
    }

    #[test]
    fn full_config_debug_redacts_all_secrets() {
        // Build a minimal `Config` by hand so we can format it as a
        // whole and verify redaction reaches every field transitively.
        let config = Config {
            web: ConfigWeb {
                address: std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0),
                allowed_origins: Vec::new(),
            },
            unix: ConfigUnix {
                socket_path: PathBuf::from("/tmp/ekaci-test.sock"),
            },
            oauth: ConfigOAuth {
                client_id: "client-id".to_string(),
                client_secret: Redacted::new("oauth-client-secret-needle".to_string()),
                redirect_url: "http://localhost/callback".to_string(),
                jwt_secret: Redacted::new("jwt-secret-needle".to_string()),
            },
            db_path: PathBuf::from("/tmp/ekaci-test.db"),
            logs_dir: PathBuf::from("/tmp/ekaci-test-logs"),
            remote_builders: Vec::new(),
            require_approval: false,
            merge_queue_require_approval: false,
            build_no_output_timeout_seconds: 1200,
            build_max_duration_seconds: 14_400,
            graph_lru_capacity: 100,
            default_merge_method: "squash".to_string(),
            caches: HashMap::new(),
            github_apps: HashMap::new(),
            security: SecurityConfig {
                max_hook_timeout_seconds: 300,
                audit_hooks: true,
                webhook_secret: Some(Redacted::new("webhook-secret-needle".to_string())),
                allow_insecure_webhooks: false,
                allow_private_cache_hosts: false,
            },
        };
        // Both the compact and pretty Debug forms must redact every secret.
        assert_no_secret(&format!("{config:?}"), "Config {:?}");
        assert_no_secret(&format!("{config:#?}"), "Config {:#?}");
    }
}

#[cfg(test)]
mod m5_tests {
    //! M5 regression: `build_max_duration_seconds` must exist on
    //! `ConfigFile` with `None` default and resolve to 14400 s when
    //! absent from `ekaci.toml`.
    use super::*;

    #[test]
    fn config_file_default_leaves_max_duration_unset() {
        let cf = ConfigFile::default();
        assert!(cf.build_max_duration_seconds.is_none());
        assert!(cf.build_no_output_timeout_seconds.is_none());
    }

    #[test]
    fn config_file_figment_roundtrip_preserves_max_duration() {
        // Use figment's Toml provider (already a dep) to parse an
        // in-memory TOML snippet overriding the field.
        use figment::providers::Format;
        let cf: ConfigFile = Figment::from(Serialized::defaults(ConfigFile::default()))
            .merge(figment::providers::Toml::string(
                "build_max_duration_seconds = 7200\n",
            ))
            .extract()
            .expect("valid toml");
        assert_eq!(cf.build_max_duration_seconds, Some(7200));
    }

    #[test]
    fn config_default_resolution_sets_14400() {
        // Mirror the logic in `Config::from_env` for the default arm
        // to lock in the 14400 s (4 h) default.
        let cf = ConfigFile::default();
        let resolved = cf.build_max_duration_seconds.unwrap_or(14_400);
        assert_eq!(resolved, 14_400);
    }
}

#[cfg(test)]
mod clamp_timeout_tests {
    use super::*;

    #[test]
    fn in_range_returns_input() {
        assert_eq!(clamp_timeout_seconds("f", 300, 1, 1000), 300);
        assert_eq!(
            clamp_timeout_seconds(
                "f",
                14_400,
                BUILD_MAX_DURATION_MIN_S,
                BUILD_MAX_DURATION_MAX_S
            ),
            14_400
        );
    }

    #[test]
    fn below_min_returns_min() {
        assert_eq!(clamp_timeout_seconds("f", 0, 30, 86_400), 30);
        assert_eq!(clamp_timeout_seconds("f", 1, 30, 86_400), 30);
        assert_eq!(clamp_timeout_seconds("f", 29, 30, 86_400), 30);
    }

    #[test]
    fn above_max_returns_max() {
        assert_eq!(clamp_timeout_seconds("f", 86_401, 30, 86_400), 86_400);
        assert_eq!(clamp_timeout_seconds("f", u64::MAX, 30, 86_400), 86_400);
    }

    #[test]
    fn boundaries_are_inclusive() {
        assert_eq!(clamp_timeout_seconds("f", 30, 30, 86_400), 30);
        assert_eq!(clamp_timeout_seconds("f", 86_400, 30, 86_400), 86_400);
    }

    #[test]
    #[should_panic(expected = "clamp_timeout_seconds")]
    fn inverted_bounds_panic_in_debug() {
        clamp_timeout_seconds("f", 100, 500, 50);
    }

    #[test]
    fn defaults_sit_inside_bounds() {
        assert!((BUILD_NO_OUTPUT_TIMEOUT_MIN_S..=BUILD_NO_OUTPUT_TIMEOUT_MAX_S).contains(&1200));
        assert!((BUILD_MAX_DURATION_MIN_S..=BUILD_MAX_DURATION_MAX_S).contains(&14_400));
        assert!((HOOK_TIMEOUT_MIN_S..=HOOK_TIMEOUT_MAX_S).contains(&default_hook_timeout()));
    }

    #[test]
    fn bounds_are_consistent() {
        assert!(BUILD_NO_OUTPUT_TIMEOUT_MIN_S < BUILD_NO_OUTPUT_TIMEOUT_MAX_S);
        assert!(BUILD_MAX_DURATION_MIN_S < BUILD_MAX_DURATION_MAX_S);
        assert!(HOOK_TIMEOUT_MIN_S < HOOK_TIMEOUT_MAX_S);
    }
}

#[cfg(test)]
mod m6_tests {
    //! M6 regression tests for `validate_cache_destination` and the
    //! underlying private-host classifier. The validator exists to
    //! prevent the operator-configured `[[caches]].destination`
    //! field from being coerced into an SSRF primitive (e.g.
    //! `http://169.254.169.254/latest/meta-data/...`) against cloud
    //! metadata or internal services.
    use super::*;

    fn allowed(dest: &str, ty: CacheType) {
        validate_cache_destination(dest, &ty, false)
            .unwrap_or_else(|e| panic!("expected {dest:?} to be allowed, got {e:?}"));
    }

    fn rejected(dest: &str, ty: CacheType) {
        let r = validate_cache_destination(dest, &ty, false);
        assert!(
            r.is_err(),
            "expected {dest:?} to be rejected, but it was accepted"
        );
    }

    // --- happy path: allowed schemes with public-looking hosts ---

    #[test]
    fn allows_s3_https_http_ssh_schemes() {
        allowed("s3://my-bucket/prefix", CacheType::NixCopy);
        allowed("https://cache.example.com", CacheType::NixCopy);
        allowed("http://cache.example.com:5000", CacheType::NixCopy);
        allowed("ssh://builder@cache.example.com", CacheType::Attic);
        allowed("ssh-ng://builder@cache.example.com", CacheType::Attic);
    }

    #[test]
    fn allows_cachix_bare_name() {
        allowed("my-cache", CacheType::Cachix);
        allowed("my.cache_1", CacheType::Cachix);
    }

    // --- scheme allow-list enforcement ---

    #[test]
    fn rejects_disallowed_schemes() {
        // LFI / RCE primitives if honoured by a downstream parser.
        rejected("file:///etc/passwd", CacheType::NixCopy);
        rejected("javascript:alert(1)", CacheType::NixCopy);
        rejected("data:text/plain,hi", CacheType::NixCopy);
        rejected("gopher://example.com", CacheType::NixCopy);
        rejected("ftp://example.com/bucket", CacheType::NixCopy);
    }

    #[test]
    fn rejects_non_cachix_without_scheme() {
        rejected("bare-name", CacheType::NixCopy);
        rejected("bare-name", CacheType::Attic);
    }

    #[test]
    fn rejects_cachix_bare_name_with_metacharacters() {
        rejected("my cache", CacheType::Cachix); // whitespace
        rejected("my;cache", CacheType::Cachix); // shell sep
        rejected("my$cache", CacheType::Cachix); // expansion
        rejected("my`cache`", CacheType::Cachix); // backtick
        rejected("my\ncache", CacheType::Cachix); // newline
    }

    #[test]
    fn rejects_empty_destination() {
        rejected("", CacheType::NixCopy);
        rejected("", CacheType::Cachix);
    }

    // --- private-IPv4 rejection (SSRF primary vector) ---

    #[test]
    fn rejects_loopback_ipv4() {
        rejected("http://127.0.0.1/", CacheType::NixCopy);
        rejected("http://127.1.2.3:8080/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_rfc1918_ipv4() {
        rejected("http://10.0.0.1/", CacheType::NixCopy);
        rejected("http://172.16.5.2/", CacheType::NixCopy);
        rejected("http://172.31.255.255/", CacheType::NixCopy);
        rejected("http://192.168.1.1/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_link_local_ipv4_and_cloud_metadata() {
        // 169.254.169.254 is EC2/GCP/Azure IMDS — the canonical SSRF
        // target. Confirm it is rejected by default.
        rejected("http://169.254.169.254/", CacheType::NixCopy);
        rejected(
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            CacheType::NixCopy,
        );
        rejected("http://169.254.0.1/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_unspecified_and_broadcast_ipv4() {
        rejected("http://0.0.0.0/", CacheType::NixCopy);
        rejected("http://255.255.255.255/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_cgnat_ipv4() {
        // 100.64.0.0/10 — carrier-grade NAT, commonly used inside
        // cloud VPC meshes.
        rejected("http://100.64.0.1/", CacheType::NixCopy);
        rejected("http://100.127.255.254/", CacheType::NixCopy);
    }

    // --- private-IPv6 rejection ---

    #[test]
    fn rejects_loopback_ipv6() {
        rejected("http://[::1]/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_link_local_ipv6() {
        rejected("http://[fe80::1]/", CacheType::NixCopy);
        rejected("http://[fe80::a00:27ff:fe4e:66a1]/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_ula_ipv6() {
        // fc00::/7 — unique-local addresses.
        rejected("http://[fc00::1]/", CacheType::NixCopy);
        rejected("http://[fd12:3456:789a::1]/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_loopback() {
        // ::ffff:127.0.0.1 — sneaky way to bypass IPv4-only checks.
        rejected("http://[::ffff:127.0.0.1]/", CacheType::NixCopy);
        rejected("http://[::ffff:7f00:1]/", CacheType::NixCopy);
    }

    // --- private-host domain rejection ---

    #[test]
    fn rejects_localhost_domain() {
        rejected("http://localhost/", CacheType::NixCopy);
        rejected("http://localhost.localdomain/", CacheType::NixCopy);
        rejected("http://LOCALHOST/", CacheType::NixCopy); // case-insensitive
    }

    #[test]
    fn rejects_gcp_metadata_domain() {
        rejected("http://metadata.google.internal/", CacheType::NixCopy);
    }

    #[test]
    fn rejects_dot_localhost_and_dot_local_suffixes() {
        rejected("http://foo.localhost/", CacheType::NixCopy);
        rejected("http://printer.local/", CacheType::NixCopy); // mDNS
    }

    // --- allow-list escape hatch ---

    #[test]
    fn allow_private_hosts_permits_loopback() {
        validate_cache_destination("http://127.0.0.1:5000", &CacheType::NixCopy, true)
            .expect("loopback must be allowed under allow_private_hosts");
        validate_cache_destination("http://localhost/", &CacheType::NixCopy, true)
            .expect("localhost must be allowed under allow_private_hosts");
        validate_cache_destination("http://[::1]/", &CacheType::NixCopy, true)
            .expect("ipv6 loopback must be allowed under allow_private_hosts");
    }

    #[test]
    fn allow_private_hosts_does_not_unlock_disallowed_schemes() {
        // Scheme check is independent of private-host flag.
        let r = validate_cache_destination("file:///etc/passwd", &CacheType::NixCopy, true);
        assert!(r.is_err());
    }

    // --- security config default ---

    #[test]
    fn allow_private_cache_hosts_defaults_to_false() {
        // Serde deserialization must yield `false` when the field is
        // absent, so existing deployments don't silently flip to
        // permissive behaviour after an upgrade.
        use figment::providers::Format;
        let sc: SecurityConfig = Figment::from(figment::providers::Toml::string(
            "max_hook_timeout_seconds = 60\naudit_hooks = true\n",
        ))
        .extract()
        .expect("valid security config");
        assert!(!sc.allow_private_cache_hosts);
    }

    #[test]
    fn allow_private_cache_hosts_roundtrips_via_toml() {
        use figment::providers::Format;
        let sc: SecurityConfig = Figment::from(figment::providers::Toml::string(
            "max_hook_timeout_seconds = 60\naudit_hooks = true\nallow_private_cache_hosts = true\n",
        ))
        .extract()
        .expect("valid security config");
        assert!(sc.allow_private_cache_hosts);
    }

    // --- no-host / malformed-URL guards ---

    #[test]
    fn rejects_malformed_https_url() {
        // Missing host entirely — `url` returns a parse error for
        // this, which the validator surfaces as a rejection.
        rejected("https://", CacheType::NixCopy);
    }

    #[test]
    fn userinfo_in_url_is_allowed() {
        // SSH URLs legitimately encode a username (`ssh://user@host`).
        // We do not replicate the CORS validator's no-userinfo
        // restriction — passwords in URLs are still a bad idea but
        // that is the operator's call, and argv carries them safely
        // (no shell expansion per H3).
        allowed("ssh://deploy-user@cache.example.com", CacheType::Attic);
    }
}
