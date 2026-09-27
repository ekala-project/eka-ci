//! MCP (Model Context Protocol) server for AI agent diagnostics.
//!
//! Exposes CI build state, PR status, build logs, and failure chains
//! as MCP tools accessible via the Streamable HTTP transport at
//! `/v1/mcp`.

mod formatting;
mod handler;
mod tools_drv;
mod tools_logs;
mod tools_pr;

use std::path::PathBuf;

use handler::McpHandler;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

use crate::db::DbService;

/// Build the MCP `StreamableHttpService` as an axum-compatible service.
///
/// The returned service manages its own session lifecycle and should be
/// mounted via `Router::nest_service("/v1/mcp", service)`.
pub(crate) fn build_mcp_service(
    db: DbService,
    logs_dir: PathBuf,
) -> StreamableHttpService<McpHandler, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(McpHandler::new(db.clone(), logs_dir.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    )
}
