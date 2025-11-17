use clap::Parser;
use serde::{self, Deserialize, Serialize};

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
    DrvStatus(Option<DrvStatusResponse>),
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
    // TODO: link to drv page
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct DrvStatusRequest {
    pub drv_path: String,
}

#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct BuildRequest {
    pub drv_path: String,
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
