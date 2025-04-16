use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use anyhow::Context;
use clap::Parser;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use tracing::info;

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

    /// Path for the frontend bundle. Frontend will be disabled if not provided.
    #[arg(short, long)]
    pub bundle_path: Option<PathBuf>,

    /// Path for the sqlite db path. Defaults to $XDG_DATA_HOME/ekaci/sqlite.db
    #[arg(short, long)]
    pub db_path: Option<PathBuf>,

    /// Path for the configuration file. Can also be set using the $EKA_CI_CONFIG_FILE.
    /// If not provided a default path will be attempted, based on the XDG spec.
    #[arg(long)]
    pub config_file: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFile {
    web: ConfigFileWeb,
    unix: ConfigFileUnix,
    db_path: Option<PathBuf>,
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

#[derive(Deserialize, Debug)]
struct ConfigEnv {
    #[serde(rename = "eka_ci_config_file")]
    pub config_file: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Config {
    pub web: ConfigWeb,
    pub unix: ConfigUnix,
    pub db_path: PathBuf,
}

#[derive(Debug)]
pub struct ConfigWeb {
    pub address: SocketAddrV4,
    pub spa_bundle: SpaBundle,
}

#[derive(Debug)]
pub enum SpaBundle {
    Disabled,
    Path(PathBuf),
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

        Ok(Config {
            web: ConfigWeb {
                address: SocketAddrV4::new(
                    args.addr
                        .or(file.web.address)
                        .unwrap_or_else(|| Ipv4Addr::new(127, 0, 0, 1)),
                    args.port.or(file.web.port).unwrap_or(3030),
                ),
                spa_bundle: args
                    .bundle_path
                    .or(file.web.bundle_path)
                    .map_or(SpaBundle::Disabled, SpaBundle::Path),
            },
            unix: ConfigUnix {
                socket_path: match args.socket.or(file.unix.socket_path) {
                    Some(p) => p,
                    None => dirs.get_runtime_file("ekaci.socket")?,
                },
            },
            db_path: args
                .db_path
                .or(file.db_path)
                .unwrap_or_else(|| dirs.get_data_file("sqlite.db")),
        })
    }
}
