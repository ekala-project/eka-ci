use serde::{Deserialize, Serialize};
use serde;
use clap::Parser;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientRequest {
  Info,
  EvalPR(EvalPRRequest),
}

// Avoid "unused parens" for default value
fn github_domain() -> String {
    "github.com".to_string()
}

#[derive(Parser, Serialize, Deserialize, Debug)]
#[command(long_about = None, arg_required_else_help = true)]
pub struct EvalPRRequest {
    #[arg(short, long, default_value_t = github_domain())]
    pub domain: String,
    #[arg(short, long)]
    pub owner: String,
    #[arg(short, long)]
    pub repo: String,
    #[arg(short, long)]
    pub number: u32,
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
pub struct EvalPRResponse {
    pub eval_id: u32,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ClientResponse {
  Info(InfoResponse),
  EvalPR(EvalPRResponse),
}

