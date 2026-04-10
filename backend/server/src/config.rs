use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use tracing::info;

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
    /// No authentication required
    None,
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

/// Security configuration for hook execution
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SecurityConfig {
    /// Maximum hook execution time in seconds (default: 300)
    #[serde(default = "default_hook_timeout")]
    pub max_hook_timeout_seconds: u64,
    /// Enable audit logging of hook executions (default: true)
    #[serde(default = "default_true")]
    pub audit_hooks: bool,
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

        // Get security config or use defaults
        let security = file.security.unwrap_or_else(|| SecurityConfig {
            max_hook_timeout_seconds: default_hook_timeout(),
            audit_hooks: true,
        });

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
            security,
        })
    }

    /// Ensure the logs directory exists, creating it if necessary
    pub fn ensure_logs_dir(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.logs_dir)?;
        Ok(())
    }
}
