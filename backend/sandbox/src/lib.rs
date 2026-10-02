pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
pub const DEFAULT_MEMORY_LIMIT_MB: u64 = 8192;

#[cfg(target_os = "linux")]
pub mod eval;
#[cfg(target_os = "linux")]
pub mod helper;
#[cfg(target_os = "linux")]
mod locate;
#[cfg(target_os = "linux")]
pub mod net;
#[cfg(target_os = "linux")]
mod runner;
#[cfg(target_os = "linux")]
mod spawn;
#[cfg(target_os = "linux")]
mod spec;

#[cfg(target_os = "linux")]
pub use {
    locate::{find_in_path, resolve_program},
    net::{Ipv4Cidr, Network, NetworkPolicy},
    runner::{HELPER_BIN, Sandbox},
    spawn::{SandboxExit, SandboxedChild},
    spec::SandboxSpec,
};
