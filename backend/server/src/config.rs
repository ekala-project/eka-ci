use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use aws_credential_types::provider::ProvideCredentials;
use clap::Parser;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

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
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFile {
    web: ConfigFileWeb,
    unix: ConfigFileUnix,
    oauth: ConfigFileOAuth,
    db_path: Option<PathBuf>,
    logs_dir: Option<PathBuf>,
    require_approval: Option<bool>,
    build_no_output_timeout_seconds: Option<u64>,
    graph_lru_capacity: Option<usize>,
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
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFileUnix {
    pub socket_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFileOAuth {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_url: Option<String>,
    pub jwt_secret: Option<String>,
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
            CredentialSource::Env { vars } => {
                debug!("Loading credentials from environment variables: {:?}", vars);
                let mut result = HashMap::new();
                for var in vars {
                    let value = std::env::var(var)
                        .with_context(|| format!("Environment variable {} not found", var))?;
                    result.insert(var.clone(), value);
                }
                Ok(result)
            },

            CredentialSource::File { path } => {
                debug!("Loading credentials from file: {}", path.display());
                let contents = tokio::fs::read_to_string(path).await.with_context(|| {
                    format!("Failed to read credential file: {}", path.display())
                })?;

                // Parse as JSON or env-style key=value pairs
                if let Ok(json_creds) = serde_json::from_str::<HashMap<String, String>>(&contents) {
                    Ok(json_creds)
                } else {
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
            },

            CredentialSource::AwsProfile { profile } => {
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
            },

            CredentialSource::CachixToken { env_var } => {
                debug!("Loading Cachix token from environment: {}", env_var);
                let token = std::env::var(env_var).with_context(|| {
                    format!("Cachix token environment variable {} not found", env_var)
                })?;

                let mut result = HashMap::new();
                result.insert("CACHIX_AUTH_TOKEN".to_string(), token);
                Ok(result)
            },

            CredentialSource::Vault {
                address,
                secret_path,
                token_env,
                namespace,
            } => {
                debug!(
                    "Loading credentials from Vault: {} at {}",
                    address, secret_path
                );

                let token = std::env::var(token_env).with_context(|| {
                    format!("Vault token environment variable {} not found", token_env)
                })?;

                // Create Vault client settings builder
                let mut settings_builder = vaultrs::client::VaultClientSettingsBuilder::default();
                settings_builder.address(address).token(&token);

                // Set namespace if provided (through settings builder)
                if let Some(ns) = namespace {
                    settings_builder.namespace(Some(ns.clone()));
                }

                let client = vaultrs::client::VaultClient::new(
                    settings_builder
                        .build()
                        .context("Failed to build Vault client settings")?,
                )
                .context("Failed to create Vault client")?;

                // Read secret from Vault KV v2
                let secret: HashMap<String, String> =
                    vaultrs::kv2::read(&client, "secret", secret_path)
                        .await
                        .with_context(|| {
                            format!("Failed to read secret from Vault path: {}", secret_path)
                        })?;

                Ok(secret)
            },

            CredentialSource::AwsSecretsManager {
                secret_name,
                region,
            } => {
                debug!(
                    "Loading credentials from AWS Secrets Manager: {}",
                    secret_name
                );

                // Load AWS config
                let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

                if let Some(r) = region {
                    config_loader = config_loader.region(aws_config::Region::new(r.clone()));
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
            },

            CredentialSource::SystemdCredential { name } => {
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
                    result.insert(name.clone(), contents.trim().to_string());
                    Ok(result)
                }
            },

            CredentialSource::InstanceMetadata => {
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
            },

            CredentialSource::GitHubAppKeyFile {
                app_id_env,
                key_file,
            } => {
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
            },

            CredentialSource::None => {
                debug!("No credentials required");
                Ok(HashMap::new())
            },
        }
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
    /// GitHub webhook secret for signature verification (optional)
    /// If not set, webhooks will be accepted without signature validation
    #[serde(default)]
    pub webhook_secret: Option<String>,
}

fn default_hook_timeout() -> u64 {
    300
}

fn default_true() -> bool {
    true
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
    pub build_no_output_timeout_seconds: u64,
    /// Maximum number of nodes in the LRU cache (default: 100,000)
    pub graph_lru_capacity: usize,
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
    pub client_secret: String,
    pub redirect_url: String,
    pub jwt_secret: String,
}

#[derive(Debug)]
pub struct ConfigWeb {
    pub address: SocketAddrV4,
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
            .or(file.oauth.client_secret)
            .unwrap_or_else(|| {
                info!("GITHUB_OAUTH_CLIENT_SECRET not set, OAuth will not work");
                String::new()
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
            .or(file.oauth.jwt_secret)
            .unwrap_or_else(|| {
                info!(
                    "JWT_SECRET not set, generating random secret (will not persist across \
                     restarts!)"
                );
                use std::time::{SystemTime, UNIX_EPOCH};
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_secs();
                format!("insecure-secret-{}", now)
            });

        // Build cache registry as a HashMap
        let caches = file
            .caches
            .into_iter()
            .map(|cache| (cache.id.clone(), cache))
            .collect::<HashMap<String, CacheConfig>>();

        // Build GitHub App registry as a HashMap
        let github_apps = file
            .github_apps
            .into_iter()
            .map(|app| (app.id.clone(), app))
            .collect::<HashMap<String, GitHubAppConfig>>();

        // Get security config or use defaults
        let mut security = file.security.unwrap_or_else(|| SecurityConfig {
            max_hook_timeout_seconds: default_hook_timeout(),
            audit_hooks: true,
            webhook_secret: None,
        });

        // Allow webhook_secret to be overridden by environment variable
        if let Ok(secret) = std::env::var("GITHUB_WEBHOOK_SECRET") {
            security.webhook_secret = Some(secret);
        }

        Ok(Config {
            web: ConfigWeb {
                address: SocketAddrV4::new(bind_addr, bind_port),
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
            build_no_output_timeout_seconds: file.build_no_output_timeout_seconds.unwrap_or(1200),
            graph_lru_capacity: file.graph_lru_capacity.unwrap_or(100_000),
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
