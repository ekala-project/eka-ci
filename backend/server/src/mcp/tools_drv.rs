use super::formatting::format_build_state;
use crate::db::DbService;
use crate::db::model::drv_id::DrvId;

pub(super) async fn get_failing_gates(
    db: &DbService,
    owner: &str,
    repo: &str,
    pr_number: i64,
) -> Result<String, String> {
    let pr = db
        .get_pull_request(owner, repo, pr_number)
        .await
        .map_err(|e| format!("Database error: {e}"))?
        .ok_or_else(|| format!("PR #{pr_number} not found in {owner}/{repo}"))?;

    let jobset_id = pr
        .jobset_id
        .ok_or("No jobset associated with this PR yet.")?;

    let drvs = db
        .get_jobset_drvs(jobset_id, None, 500, 0)
        .await
        .map_err(|e| format!("Database error: {e}"))?;

    let failing: Vec<_> = drvs.iter().filter(|d| d.build_state.is_failure()).collect();

    if failing.is_empty() {
        return Ok(format!(
            "No failing gates for PR #{pr_number} in {owner}/{repo}."
        ));
    }

    let mut lines = Vec::with_capacity(failing.len());
    for drv in &failing {
        lines.push(format!(
            "  {} ({}) — {} [{}]",
            drv.name,
            drv.system,
            format_build_state(&drv.build_state),
            &*drv.drv_path,
        ));
    }

    Ok(format!(
        "Failing gates for PR #{} ({} of {} drvs):\n{}",
        pr_number,
        failing.len(),
        drvs.len(),
        lines.join("\n"),
    ))
}

pub(super) async fn get_drv_details(db: &DbService, drv_path_str: &str) -> Result<String, String> {
    let drv_id = parse_drv(drv_path_str)?;

    let drv = db
        .get_drv(&drv_id)
        .await
        .map_err(|e| format!("Database error: {e}"))?
        .ok_or_else(|| format!("Derivation not found: {drv_path_str}"))?;

    let mut output = format!(
        "Derivation: {}\nSystem: {}\nBuild state: {}\nFOD: {}\n",
        &*drv.drv_path,
        drv.system,
        format_build_state(&drv.build_state),
        drv.is_fod,
    );

    if let Some(ref pname) = drv.pname {
        output.push_str(&format!("Package: {}", pname));
        if let Some(ref version) = drv.version {
            output.push_str(&format!(" {version}"));
        }
        output.push('\n');
    }

    if let Some(size) = drv.output_size {
        output.push_str(&format!("Output size: {} bytes\n", size));
    }
    if let Some(size) = drv.closure_size {
        output.push_str(&format!("Closure size: {} bytes\n", size));
    }
    if let Some(ref pos) = drv.meta_position {
        output.push_str(&format!("Source position: {pos}\n"));
    }
    if drv.broken == Some(true) {
        output.push_str("Marked broken: yes\n");
    }
    if drv.insecure == Some(true) {
        output.push_str("Marked insecure: yes\n");
    }

    Ok(output)
}

pub(super) async fn get_failure_chain(
    db: &DbService,
    drv_path_str: &str,
) -> Result<String, String> {
    let drv_id = parse_drv(drv_path_str)?;

    let drv = db
        .get_drv(&drv_id)
        .await
        .map_err(|e| format!("Database error: {e}"))?
        .ok_or_else(|| format!("Derivation not found: {drv_path_str}"))?;

    if !drv.build_state.is_failure() {
        return Ok(format!(
            "{} is not in a failure state (current: {}).",
            drv_path_str,
            format_build_state(&drv.build_state),
        ));
    }

    if drv.build_state != crate::db::model::build_event::DrvBuildState::TransitiveFailure {
        return Ok(format!(
            "{} failed directly ({}).\nUse get_build_log to see the build output.",
            drv_path_str,
            format_build_state(&drv.build_state),
        ));
    }

    let root_causes = db
        .get_failed_dependencies(&drv_id)
        .await
        .map_err(|e| format!("Database error: {e}"))?;

    if root_causes.is_empty() {
        return Ok(format!(
            "{drv_path_str} is marked as transitive failure but no root cause was found in the \
             database."
        ));
    }

    let mut lines = Vec::new();
    for cause_id in &root_causes {
        match db.get_drv(cause_id).await {
            Ok(Some(cause_drv)) => {
                let name = cause_drv
                    .pname
                    .as_deref()
                    .unwrap_or_else(|| &*cause_drv.drv_path);
                lines.push(format!(
                    "  {} — {} [{}]",
                    name,
                    format_build_state(&cause_drv.build_state),
                    &*cause_drv.drv_path,
                ));
            },
            Ok(None) => {
                lines.push(format!("  {} — (details not found)", &**cause_id));
            },
            Err(e) => {
                lines.push(format!("  {} — (error: {})", &**cause_id, e));
            },
        }
    }

    Ok(format!(
        "{} failed due to transitive dependency failure.\nRoot cause(s):\n{}",
        drv_path_str,
        lines.join("\n"),
    ))
}

fn parse_drv(drv_path_str: &str) -> Result<DrvId, String> {
    DrvId::try_from(drv_path_str).map_err(|_| {
        format!(
            "Invalid derivation path: '{drv_path_str}'. Expected format: 'hash-name.drv' or \
             '/nix/store/hash-name.drv'"
        )
    })
}
