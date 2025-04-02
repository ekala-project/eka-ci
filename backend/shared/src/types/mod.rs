use serde;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientRequest {
    Info,
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
}
