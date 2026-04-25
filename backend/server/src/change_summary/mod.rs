//! Package change summary and rebuild impact analysis.
//!
//! This module implements features A1 (package change summary) and A2
//! (rebuild impact analysis) described in
//! `docs/design-package-change-rebuild-impact.md`.
//!
//! ## Phases
//!
//! - **P1 (this commit)**: [`classify`] — derives a list of [`PackageChange`] entries for a
//!   `(head_sha, base_sha, job)` triple by joining `Job` and `Drv` rows and matching by `Job.name`
//!   (attr_path).
//! - **P2**: `impact` — rebuild count + multi-source BFS for blast radius.
//! - **P4**: `render` — markdown rendering for GitHub check posting.
//!
//! ## Public API surface (P1)
//!
//! - [`classify::compute_package_changes`] — pure-logic classification driven off two
//!   `Vec<JobDrvRow>` inputs (head + base).
//! - [`classify::load_job_drv_rows`] — DB loader that produces `JobDrvRow`s for a given jobset id.
//! - [`PackageChange`], [`PackageChangesResponse`] — wire types for the new
//!   `/v1/commits/{sha}/package-changes` endpoint.

pub mod cache;
pub mod classify;
pub mod impact;
pub mod types;

// `PackageChange` is part of the public API surface for downstream phases
// (P2 impact + P4 render); the bin target doesn't reach for it directly
// yet, hence the explicit allow.
use anyhow::Context;
use sqlx::{Pool, Sqlite};
#[allow(unused_imports)]
pub use types::{
    PackageChange, PackageChangesResponse, PerSystemImpact, RebuildImpactResponse,
    TopBlastRadiusEntry,
};

/// Build a [`PackageChangesResponse`] for a `(head_sha, base_sha, job)`
/// triple by:
///
/// 1. Resolving both SHAs to jobset ids in `GitHubJobSets`.
/// 2. Loading `Job ⋈ Drv` rows for each side via [`classify::load_job_drv_rows`].
/// 3. Calling [`classify::compute_package_changes`] for the structured diff.
///
/// Returns `Ok(None)` when the **head** jobset is missing — the caller
/// should map this to a 404. A missing **base** jobset is treated as
/// "everything is new" (every head row classifies as `Added`), matching
/// the semantics of [`crate::db::github::job_difference`].
pub async fn build_package_changes_response(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_packages_listed: usize,
) -> anyhow::Result<Option<PackageChangesResponse>> {
    let head_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve head jobset id")?;

    let Some(head_jobset_id) = head_jobset_id else {
        return Ok(None);
    };

    let base_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(base_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve base jobset id")?;

    let head_rows = classify::load_job_drv_rows(pool, head_jobset_id).await?;
    let base_rows = match base_jobset_id {
        Some(id) => classify::load_job_drv_rows(pool, id).await?,
        None => Vec::new(),
    };

    let (mut changes, metadata_available) =
        classify::compute_package_changes(&head_rows, &base_rows);

    let truncated = changes.len() > max_packages_listed;
    if truncated {
        changes.truncate(max_packages_listed);
    }

    let computed_at = chrono::Utc::now().to_rfc3339();

    Ok(Some(PackageChangesResponse {
        head_sha: head_sha.to_string(),
        base_sha: base_sha.to_string(),
        job: job.to_string(),
        computed_at,
        metadata_available,
        package_changes: changes,
        truncated,
    }))
}

/// Default truncation limit per design §11.1 (`max_packages_listed`).
pub const DEFAULT_MAX_PACKAGES_LISTED: usize = 100;

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::SqlitePool;

    use super::*;
    use crate::db::github::{create_jobs_for_jobset, create_jobset};
    use crate::db::model::DrvId;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::{Drv, insert_drv};
    use crate::nix::nix_eval_jobs::NixEvalDrv;

    /// Build a [`NixEvalDrv`] sufficient to satisfy `create_jobs_for_jobset`.
    /// This is the *evaluation-shape* counterpart of the DB-shape `Drv`
    /// inserted by `make_db_drv`.
    fn make_eval(attr: &str, drv_path: &str, name: &str) -> NixEvalDrv {
        NixEvalDrv {
            attr: attr.to_string(),
            attr_path: vec![attr.to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: None,
            name: name.to_string(),
            outputs: std::collections::HashMap::new(),
            system: "x86_64-linux".to_string(),
            meta: None,
        }
    }

    /// Build a `Drv` row with the package metadata fields populated.
    fn make_db_drv(
        drv_path: &str,
        pname: Option<&str>,
        version: Option<&str>,
        license_json: Option<&str>,
        maintainers_json: Option<&str>,
    ) -> Drv {
        Drv {
            drv_path: DrvId::from_str(drv_path).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: pname.map(str::to_string),
            version: version.map(str::to_string),
            license_json: license_json.map(str::to_string),
            maintainers_json: maintainers_json.map(str::to_string),
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }

    /// 32-char store hash placeholders that satisfy `DrvId` validation.
    const H_BASE: &str = "1111111111111111111111111111111";
    const H_HEAD: &str = "2222222222222222222222222222222";

    fn drv_path(hash_prefix: &str, name: &str) -> String {
        // Pad the variable hash to a 32-char store hash so DrvId accepts it.
        format!(
            "/nix/store/{hash_prefix}{filler}-{name}.drv",
            // 32 alphanumeric chars total: prefix is 31 → pad with one.
            filler = "z"
        )
    }

    /// Set up a (sha, job) jobset with the supplied (eval_drv, db_drv) rows.
    async fn make_jobset(
        pool: &SqlitePool,
        sha: &str,
        job: &str,
        rows: &[(NixEvalDrv, Drv)],
    ) -> i64 {
        let jobset_id = create_jobset(sha, job, "owner", "repo", None, pool)
            .await
            .expect("create_jobset failed");
        for (_eval, db_drv) in rows {
            insert_drv(pool, db_drv).await.expect("insert_drv failed");
        }
        let evals: Vec<NixEvalDrv> = rows.iter().map(|(e, _)| e.clone()).collect();
        create_jobs_for_jobset(jobset_id, &evals, pool)
            .await
            .expect("create_jobs_for_jobset failed");
        jobset_id
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_returns_none_when_head_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let resp =
            build_package_changes_response(&pool, "missing-sha", "base-sha", "ci", 100).await?;
        assert!(resp.is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_emits_added_when_base_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let head_drv_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_drv_path, "hello-2.13");
        let head_db = make_db_drv(&head_drv_path, Some("hello"), Some("2.13"), None, None);

        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let resp = build_package_changes_response(&pool, "head-sha", "missing-base", "ci", 100)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.head_sha, "head-sha");
        assert_eq!(resp.package_changes.len(), 1);
        assert!(matches!(
            resp.package_changes[0],
            PackageChange::Added { .. }
        ));
        assert!(resp.metadata_available);
        assert!(!resp.truncated);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_classifies_version_bump_end_to_end(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let base_path = drv_path(H_BASE, "hello-2.12");
        let head_path = drv_path(H_HEAD, "hello-2.13");

        let base_eval = make_eval("hello", &base_path, "hello-2.12");
        let base_db = make_db_drv(&base_path, Some("hello"), Some("2.12"), None, None);
        make_jobset(&pool, "base-sha", "ci", &[(base_eval, base_db)]).await;

        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let resp = build_package_changes_response(&pool, "head-sha", "base-sha", "ci", 100)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.package_changes.len(), 1);
        match &resp.package_changes[0] {
            PackageChange::VersionBump {
                pname, old, new, ..
            } => {
                assert_eq!(pname, "hello");
                assert_eq!(old, "2.12");
                assert_eq!(new, "2.13");
            },
            other => panic!("expected VersionBump, got {other:?}"),
        }
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_truncates_when_over_limit(pool: SqlitePool) -> anyhow::Result<()> {
        // Five "Added" entries; truncate at 2.
        let mut rows = Vec::new();
        for i in 0..5 {
            // Use a fresh, valid 32-char hash for each row by appending a
            // changing suffix and re-padding.
            let hash = format!("{:0>32}", format!("a{i}"));
            let path = format!("/nix/store/{hash}-pkg{i}-1.0.drv");
            let eval = make_eval(&format!("pkg{i}"), &path, &format!("pkg{i}-1.0"));
            let db = make_db_drv(&path, Some(&format!("pkg{i}")), Some("1.0"), None, None);
            rows.push((eval, db));
        }
        make_jobset(&pool, "head-sha", "ci", &rows).await;

        let resp = build_package_changes_response(&pool, "head-sha", "base-sha", "ci", 2)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.package_changes.len(), 2);
        assert!(resp.truncated);
        Ok(())
    }
}
