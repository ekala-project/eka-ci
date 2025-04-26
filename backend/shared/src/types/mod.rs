use clap::Parser;
use serde;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientRequest {
    Info,
    Build(BuildRequest),
    Job(JobRequest),
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
#[serde(tag = "type")]
pub enum ClientResponse {
    Info(InfoResponse),
    Build(BuildResponse),
    Job(JobResponse),
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

// TODO: We should probably just have a generic async "event received" response
#[derive(Serialize, Parser, Deserialize, Debug)]
pub struct JobResponse {
    pub enqueued: bool,
}
