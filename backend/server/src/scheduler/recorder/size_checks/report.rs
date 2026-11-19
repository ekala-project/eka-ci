//! Assemble the [`BuildReport`] shown on a successful build's check runs.

use std::collections::HashMap;

use sqlx::SqlitePool;
use tracing::warn;

use crate::db::model::DrvId;
use crate::db::size::SizeComparison;
use crate::github::service::build_report::{BuildReport, OutputReport};
use crate::nix::size::OutputSizeReport;

/// The build's measurements, gathered by the size checker.
pub(super) struct Measured<'a> {
    /// Output name → store path.
    pub outputs: &'a HashMap<String, String>,
    /// Sizes, if the outputs could be measured.
    pub sizes: Option<&'a OutputSizeReport>,
    /// Built by eka-ci rather than substituted.
    pub built_locally: bool,
    /// Baseline comparisons of the jobs the derivation belongs to.
    pub comparisons: Vec<SizeComparison>,
}

/// Combine `measured` with the derivation's metadata and build time.
/// Database lookups that fail are logged and left out of the report.
pub(super) async fn build_report(
    pool: &SqlitePool,
    drv_id: &DrvId,
    measured: Measured<'_>,
) -> BuildReport {
    let mut report = BuildReport {
        built_locally: measured.built_locally,
        outputs: output_reports(measured.outputs, measured.sizes),
        total_output_size: measured.sizes.map(|s| s.total_nar_size),
        total_closure_size: measured.sizes.map(|s| s.total_closure_size),
        comparisons: measured.comparisons,
        ..Default::default()
    };
    report.sort_outputs();

    match crate::db::model::drv::get_drv(drv_id, pool).await {
        Ok(Some(drv)) => {
            report.system = drv.system;
            report.package = package_label(drv.pname, drv.version);
        },
        Ok(None) => {},
        Err(e) => warn!(
            "No Drv row for build report of {}: {}",
            drv_id.store_path(),
            e
        ),
    }
    if measured.built_locally {
        report.duration_secs = build_duration(pool, drv_id).await.unwrap_or_else(|e| {
            warn!(
                "Failed to read build duration of {}: {}",
                drv_id.store_path(),
                e
            );
            None
        });
    }
    report
}

fn output_reports(
    outputs: &HashMap<String, String>,
    sizes: Option<&OutputSizeReport>,
) -> Vec<OutputReport> {
    outputs
        .iter()
        .map(|(name, path)| {
            let size = sizes.and_then(|s| s.outputs.get(path));
            OutputReport {
                name: name.clone(),
                path: path.clone(),
                nar_size: size.map(|s| s.nar_size),
                closure_size: size.map(|s| s.closure_size),
                closure_paths: size.map(|s| s.closure_paths),
            }
        })
        .collect()
}

/// `hello 2.12.1`, or just the name when there is no version.
fn package_label(pname: Option<String>, version: Option<String>) -> Option<String> {
    match (pname, version) {
        (Some(pname), Some(version)) => Some(format!("{pname} {version}")),
        (Some(pname), None) => Some(pname),
        (None, _) => None,
    }
}

/// Seconds between the derivation's last `Building` event and the success
/// that followed it, from the `DrvBuildEvent` audit log.
pub(super) async fn build_duration(
    pool: &SqlitePool,
    drv_id: &DrvId,
) -> anyhow::Result<Option<i64>> {
    use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};

    let duration: Option<i64> = sqlx::query_scalar(
        r#"
        SELECT done.timestamp - start.timestamp
        FROM DrvBuildEvent AS done
        JOIN DrvBuildEvent AS start
          ON start.derivation = done.derivation AND start.rowid < done.rowid
        WHERE done.derivation = ?1 AND done.state = ?2 AND start.state = ?3
        ORDER BY done.rowid DESC, start.rowid DESC
        LIMIT 1
        "#,
    )
    .bind(drv_id.store_path())
    .bind(DrvBuildState::Completed(DrvBuildResult::Success))
    .bind(DrvBuildState::Building)
    .fetch_optional(pool)
    .await?;
    Ok(duration.filter(|secs| *secs >= 0))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
    use crate::nix::size::OutputSize;

    const DRV: &str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";

    async fn event(pool: &SqlitePool, state: DrvBuildState, at: i64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO DrvBuildEvent (derivation, build_attempt, state, timestamp) VALUES (?, \
             1, ?, ?)",
        )
        .bind(DrvId::from_str(DRV)?.store_path())
        .bind(state)
        .bind(at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// A retried build is timed from its last start, not its first.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn duration_is_measured_from_the_last_start(pool: SqlitePool) -> anyhow::Result<()> {
        let drv = DrvId::from_str(DRV)?;
        assert_eq!(build_duration(&pool, &drv).await?, None);

        event(&pool, DrvBuildState::Queued, 0).await?;
        event(&pool, DrvBuildState::Building, 100).await?;
        event(&pool, DrvBuildState::FailedRetry, 150).await?;
        event(&pool, DrvBuildState::Building, 200).await?;
        assert_eq!(build_duration(&pool, &drv).await?, None);

        event(
            &pool,
            DrvBuildState::Completed(DrvBuildResult::Success),
            290,
        )
        .await?;
        assert_eq!(build_duration(&pool, &drv).await?, Some(90));
        Ok(())
    }

    #[test]
    fn outputs_carry_their_sizes() {
        let outputs = HashMap::from([
            ("out".to_string(), "/nix/store/aaa-hello".to_string()),
            ("man".to_string(), "/nix/store/bbb-hello-man".to_string()),
        ]);
        let size = OutputSize {
            nar_size: 10,
            closure_size: 100,
            closure_paths: 3,
        };
        let sizes = OutputSizeReport {
            outputs: HashMap::from([("/nix/store/aaa-hello".to_string(), size)]),
            total_nar_size: 10,
            total_closure_size: 100,
        };
        let mut reports = output_reports(&outputs, Some(&sizes));
        reports.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(reports[0].name, "man");
        assert_eq!(reports[0].nar_size, None);
        assert_eq!(reports[1].closure_paths, Some(3));
        assert_eq!(
            package_label(Some("hello".into()), None).as_deref(),
            Some("hello")
        );
    }
}
