use std::path::Path;

use tracing::error;

use super::formatting::truncate_log;
use crate::db::model::drv_id::DrvId;
use crate::web::security::path_is_under;

pub(super) async fn get_build_log(
    logs_dir: &Path,
    drv_path_str: &str,
    tail_lines: Option<usize>,
) -> Result<String, String> {
    let drv_id = DrvId::try_from(drv_path_str).map_err(|_| {
        format!(
            "Invalid derivation path: '{drv_path_str}'. Expected format: 'hash-name.drv' or \
             '/nix/store/hash-name.drv'"
        )
    })?;

    let log_path = logs_dir.join(drv_id.drv_hash()).join("build.log");

    if !path_is_under(logs_dir, &log_path) {
        error!(
            "MCP: refusing to serve log outside logs_dir: {}",
            log_path.display()
        );
        return Err("Internal error: path validation failed.".to_string());
    }

    let contents = tokio::fs::read_to_string(&log_path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "Build log not found for derivation: {}",
                drv_id.store_path()
            )
        } else {
            error!("MCP: failed to read log for {}: {}", drv_id.store_path(), e);
            format!("Failed to read build log: {e}")
        }
    })?;

    let lines = tail_lines.unwrap_or(200);
    Ok(truncate_log(&contents, lines))
}
