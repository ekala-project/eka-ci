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

    #[arg(long)]
    pub config_file: Option<PathBuf>,

    #[arg(long)]
    pub cli_only: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigFile {
    web: ConfigFileWeb,
    unix: ConfigFileUnix,
    file_only: Option<String>,
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
    pub env_only: Option<String>,
}

#[derive(Debug)]
pub struct Config {
    web: ConfigWeb,
    unix: ConfigUnix,
    cli_only: Option<String>,
    file_only: Option<String>,
    env_only: Option<String>,
}

#[derive(Debug)]
struct ConfigWeb {
    address: SocketAddrV4,
    spa_bundle: SpaBundle,
}

#[derive(Debug)]
enum SpaBundle {
    Disabled,
    Path(PathBuf),
}

#[derive(Debug)]
struct ConfigUnix {
    socket_path: PathBuf,
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

        let file = Figment::from(Serialized::defaults(ConfigFile::default()))
            .merge(Toml::file(config_path))
            .merge(Env::prefixed("EKA_CI_").split("__"))
            .extract::<ConfigFile>().context("failed to parse config file")?;

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
            cli_only: args.cli_only,
            env_only: env.env_only,
            file_only: file.file_only,
        })
    }
}
