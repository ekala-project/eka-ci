use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::db::DbService;

#[derive(Clone)]
pub(crate) struct McpHandler {
    pub(crate) db: DbService,
    pub(crate) logs_dir: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl McpHandler {
    pub(crate) fn new(db: DbService, logs_dir: PathBuf) -> Self {
        Self {
            db,
            logs_dir,
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListFailingPrsParams {
    /// Optional repo filter (e.g. 'owner/repo' or partial match)
    repo: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PrParams {
    /// Repository owner
    owner: String,
    /// Repository name
    repo: String,
    /// PR number
    pr_number: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BuildLogParams {
    /// Derivation path (e.g. 'hash-name.drv' or full /nix/store/ path)
    drv_path: String,
    /// Number of lines from the end to return (default: 200)
    tail_lines: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DrvParams {
    /// Derivation path (e.g. 'hash-name.drv' or full /nix/store/ path)
    drv_path: String,
}

#[tool_router]
impl McpHandler {
    #[tool(
        description = "List open PRs that have failing CI gates. Optionally filter by repository \
                       name."
    )]
    async fn list_failing_prs(
        &self,
        Parameters(params): Parameters<ListFailingPrsParams>,
    ) -> String {
        match super::tools_pr::list_failing_prs(&self.db, params.repo.as_deref()).await {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Get build status summary for a specific PR.")]
    async fn get_pr_status(&self, Parameters(params): Parameters<PrParams>) -> String {
        match super::tools_pr::get_pr_status(
            &self.db,
            &params.owner,
            &params.repo,
            params.pr_number,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "List failed derivations (gates) for a PR. Shows direct failures, \
                       transitive failures, and interrupted builds."
    )]
    async fn get_failing_gates(&self, Parameters(params): Parameters<PrParams>) -> String {
        match super::tools_drv::get_failing_gates(
            &self.db,
            &params.owner,
            &params.repo,
            params.pr_number,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "Get the build log for a derivation. Returns the last N lines (default 200)."
    )]
    async fn get_build_log(&self, Parameters(params): Parameters<BuildLogParams>) -> String {
        match super::tools_logs::get_build_log(&self.logs_dir, &params.drv_path, params.tail_lines)
            .await
        {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        description = "For a transitively-failed derivation, find the root cause failure(s) in \
                       the dependency chain."
    )]
    async fn get_failure_chain(&self, Parameters(params): Parameters<DrvParams>) -> String {
        match super::tools_drv::get_failure_chain(&self.db, &params.drv_path).await {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(description = "Get detailed metadata and build state for a specific derivation.")]
    async fn get_drv_details(&self, Parameters(params): Parameters<DrvParams>) -> String {
        match super::tools_drv::get_drv_details(&self.db, &params.drv_path).await {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "EKA-CI build system diagnostics. Use these tools to investigate failing PR gates, \
             read build logs, and trace dependency failures.",
        )
    }
}
