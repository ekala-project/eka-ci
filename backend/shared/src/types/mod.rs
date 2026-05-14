use clap::Parser;
use serde::{self, Deserialize, Serialize};

// Derivation-related types
mod build_state;
mod drv;
mod drv_id;

pub use build_state::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};
pub use drv::Drv;
pub use drv_id::{DrvId, InvalidDrvId, Reference, Referrer, strip_store_path};

// Jobset-related types
mod jobset_data;

pub use jobset_data::JobsetData;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientRequest {
    Info,
    Build(BuildRequest),
    Job(JobRequest),
    Repo(RepoRequest),
    Git(GitRequest),
    GitHub { pr: GitHubPrRequest },
    DrvStatus(DrvStatusRequest),
    ChannelStatus(ChannelStatusRequest),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerStatus {
    Active,
    Degraded,
    Dead,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InfoResponse {
    pub status: ServerStatus,
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug)]
//#[serde(tag = "type")]
pub enum ClientResponse {
    Info(InfoResponse),
    Ack(bool),
    DrvStatus(Result<DrvStatusResponse, String>),
    ChannelStatus(Result<ChannelStatusResponse, String>),
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct GitRequest {
    pub domain: String,
    pub owner: String,
    pub repo: String,
    pub commitish: String,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct GitHubPrRequest {
    pub domain: String,
    pub owner: String,
    pub repo: String,
    pub pr: u64,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct DrvStatusResponse {
    pub drv_path: String,
    pub status: String,
    pub failed_dependencies: Option<Vec<String>>,
    // TODO: link to drv page
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct DrvStatusRequest {
    pub drv_path: String,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct BuildRequest {
    pub drv_path: String,
    /// Force rebuild of failed derivations (resets to Queued state)
    #[arg(short, long)]
    #[serde(default)]
    pub force: bool,
    /// Rebuild all failed derivations (requires --force)
    #[arg(short = 'a', long)]
    #[serde(default)]
    pub rebuild_all: bool,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct BuildResponse {
    pub enqueued: bool,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct JobRequest {
    pub file_path: String,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct RepoRequest {
    pub file_path: String,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct ChannelStatusRequest {
    /// Channel name to query (e.g., "stable", "unstable")
    pub channel_name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ChannelPromotion {
    pub tracking_sha: String,
    pub target_branch: String,
    pub status: String,
    pub created_at: String,
    pub blocked_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ChannelStatusResponse {
    pub channel_id: String,
    pub in_flight: Option<ChannelPromotion>,
    pub recent_promotions: Vec<ChannelPromotion>,
}
