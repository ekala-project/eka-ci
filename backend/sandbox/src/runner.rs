use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

use crate::locate::find_in_path;
use crate::net::{self, Network, NetworkPolicy};
use crate::spawn::{self, SandboxExit, SandboxedChild};
use crate::spec::SandboxSpec;
use crate::{helper, locate};

pub const HELPER_BIN: &str = "ekaci-sandbox-helper";

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Sandbox {
    bwrap: PathBuf,
    helper: PathBuf,
    pasta: Option<PathBuf>,
    policy: NetworkPolicy,
    probed: OnceCell<()>,
    net_probed: OnceCell<()>,
}

impl Sandbox {
    pub fn locate(helper: Option<&Path>) -> Result<Self> {
        let bwrap = find_in_path("bwrap").context(
            "bubblewrap (`bwrap`) not found in PATH; the evaluation sandbox requires it",
        )?;
        let helper = match helper {
            Some(p) => p.to_path_buf(),
            None => locate::default_helper()?,
        };
        let sandbox = Self::with_paths(bwrap, helper);
        Ok(match find_in_path("pasta") {
            Some(pasta) => sandbox.with_pasta(pasta),
            None => sandbox,
        })
    }

    pub fn with_paths(bwrap: PathBuf, helper: PathBuf) -> Self {
        Self {
            bwrap,
            helper,
            pasta: None,
            policy: NetworkPolicy::default(),
            probed: OnceCell::new(),
            net_probed: OnceCell::new(),
        }
    }

    pub fn with_pasta(mut self, pasta: PathBuf) -> Self {
        self.pasta = Some(pasta);
        self
    }

    pub fn with_network_policy(mut self, policy: NetworkPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub async fn spawn(&self, spec: &SandboxSpec) -> Result<SandboxedChild> {
        let cmd = match spec.network_value() {
            Network::None => {
                self.preflight().await?;
                self.bwrap_command(spec)
            },
            Network::Filtered => {
                self.preflight_network().await?;
                self.filtered_command(spec)?
            },
        };
        spawn::spawn_supervised(cmd, spec.timeout_value()).await
    }

    fn bwrap_command(&self, spec: &SandboxSpec) -> Command {
        let mut cmd = Command::new(&self.bwrap);
        cmd.args(spec.bwrap_args(&self.helper));
        cmd
    }

    fn filtered_command(&self, spec: &SandboxSpec) -> Result<Command> {
        let pasta = self.pasta.as_ref().context(
            "passt (`pasta`) not found in PATH; it is required for sandboxes with network access",
        )?;
        let host = net::host::ipv4_addrs().context("failed to list host IPv4 addresses")?;
        let plan = net::route_plan(&self.policy, &host);
        let mut cmd = Command::new(&self.helper);
        cmd.args(net::attach_args(pasta, &plan))
            .arg("--")
            .arg(&self.bwrap)
            .args(spec.bwrap_args(&self.helper));
        Ok(cmd)
    }

    pub async fn preflight(&self) -> Result<()> {
        self.probed
            .get_or_try_init(|| self.probe(Network::None))
            .await?;
        Ok(())
    }

    pub async fn preflight_network(&self) -> Result<()> {
        self.preflight().await?;
        self.net_probed
            .get_or_try_init(|| self.probe(Network::Filtered))
            .await?;
        Ok(())
    }

    async fn probe(&self, network: Network) -> Result<()> {
        let mut spec = SandboxSpec::new(&self.helper).network(network);
        spec.probe = true;
        let cmd = match network {
            Network::None => self.bwrap_command(&spec),
            Network::Filtered => self.filtered_command(&spec)?,
        };
        let mut child = spawn::spawn_supervised(cmd, Some(PROBE_TIMEOUT)).await?;
        let (stdout, stderr) = read_all(&mut child).await;
        let exit = child.wait().await?;
        if !matches!(exit, SandboxExit::Exited(s) if s.success()) {
            bail!(
                "sandbox preflight ({network:?} network) failed ({exit:?}); bwrap and pasta need \
                 unprivileged user namespaces (check kernel.unprivileged_userns_clone / \
                 user.max_user_namespaces and systemd RestrictNamespaces), pasta also \
                 /dev/net/tun: {}",
                stderr.trim()
            );
        }
        if network == Network::None {
            report_landlock(&stdout);
        } else {
            info!("sandbox network filter ready (pasta)");
        }
        Ok(())
    }
}

fn report_landlock(probe_stdout: &str) {
    let level = probe_stdout
        .lines()
        .find_map(|l| l.strip_prefix(helper::PROBE_PREFIX))
        .unwrap_or("unknown");
    match level {
        "full" => info!("sandbox ready: bwrap + landlock fully enforced"),
        "partial" => info!("sandbox ready: bwrap + landlock (partially enforced by kernel)"),
        other => warn!(
            landlock = other,
            "landlock is not available on this kernel; sandbox degrades to bwrap isolation only"
        ),
    }
}

async fn read_all(child: &mut SandboxedChild) -> (String, String) {
    let (mut out, mut err) = (String::new(), String::new());
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let out_fut = async {
        if let Some(mut s) = stdout {
            if let Err(e) = s.read_to_string(&mut out).await {
                debug!("reading sandbox probe output failed: {e}");
            }
        }
    };
    let err_fut = async {
        if let Some(mut s) = stderr {
            if let Err(e) = s.read_to_string(&mut err).await {
                debug!("reading sandbox probe output failed: {e}");
            }
        }
    };
    tokio::join!(out_fut, err_fut);
    (out, err)
}
