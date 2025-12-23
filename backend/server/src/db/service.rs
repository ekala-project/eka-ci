use std::path::Path;

use sqlx::migrate;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tracing::{debug, info};

use super::model::drv::Drv;
use super::model::drv_id::DrvId;
use super::model::{build_event, drv};
use super::{approved_users, github, github_installations};
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Clone)]
pub struct DbService {
    // Instead of exposing this, we should probably have a function
    // where people can get a cloned instance
    pub pool: SqlitePool,
}

impl DbService {
    pub async fn new(location: &Path) -> anyhow::Result<DbService> {
        info!("Initializing SQLite database at {}", location.display());

        // SQlite does itself not create any directories, so we need to ensure the parent of the
        // database path already exists before creating the pool.
        // If the path has no parent (because it is directly under the root for example), we just
        // assume that the parent already exists. If it did in fact not, the SQlite pool creation
        // will fail, but such cases are considered a user error.
        if let Some(parent) = location.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(location)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        debug!("Creating database pool with {:?}", opts);

        let pool: SqlitePool = SqlitePool::connect_with(opts).await?;

        info!("Running database migrations");
        migrate!("sql/migrations").run(&pool).await?;

        Ok(DbService { pool })
    }

    //pub async fn insert_build(
    //    &self,
    //    metadata: ForInsert<DrvBuildMetadata>,
    //) -> anyhow::Result<DrvBuildMetadata> {
    //    insert::new_drv_build_metadata(metadata, &self.pool).await
    //}

    pub async fn get_drv(&self, drv_path: &DrvId) -> anyhow::Result<Option<drv::Drv>> {
        drv::get_drv(drv_path, &self.pool).await
    }

    //pub async fn has_drv(&self, drv_path: &str) -> anyhow::Result<bool> {
    //    drv::has_drv(&self.pool, drv_path).await
    //}

    //pub async fn drv_references(&self, drv: &DrvId) -> anyhow::Result<Vec<drv::Drv>> {
    //    drv::drv_references(&self.pool, &drv).await
    //}

    pub async fn drv_referrers(&self, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        drv::drv_referrers(&self.pool, drv).await
    }

    pub async fn get_all_transitive_referrers(&self, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        drv::get_all_transitive_referrers(drv, &self.pool).await
    }

    pub async fn insert_transitive_failures(
        &self,
        failed_drv: &DrvId,
        transitive_referrers: &[DrvId],
    ) -> anyhow::Result<()> {
        drv::insert_transitive_failures(failed_drv, transitive_referrers, &self.pool).await
    }

    pub async fn clear_transitive_failures(&self, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        drv::clear_transitive_failures(drv, &self.pool).await
    }

    pub async fn get_failed_dependencies(&self, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        drv::get_failed_dependencies(drv, &self.pool).await
    }

    pub async fn insert_drvs_and_references(
        &self,
        drvs: &[Drv],
        drv_refs: &[(DrvId, DrvId)],
    ) -> anyhow::Result<()> {
        drv::insert_drvs_and_references(&self.pool, drvs, drv_refs).await
    }

    //pub async fn insert_drv_graph(
    //    &self,
    //    drv_graph: &HashMap<DrvId, Vec<DrvId>>,
    //) -> anyhow::Result<()> {
    //    drv::insert_drv_graph(&self.pool, drv_graph).await
    //}

    // pub async fn new_drv_build_event(
    //     &self,
    //     event: ForInsert<build_event::DrvBuildEvent>,
    // ) -> anyhow::Result<build_event::DrvBuildEvent> {
    //     insert::new_drv_build_event(event, &self.pool).await
    // }

    pub async fn update_drv_status(
        &self,
        drv_id: &DrvId,
        state: &build_event::DrvBuildState,
    ) -> anyhow::Result<()> {
        drv::update_drv_status(&self.pool, drv_id, state).await
    }

    pub async fn is_drv_buildable(&self, derivation: &DrvId) -> anyhow::Result<bool> {
        build_event::is_drv_buildable(derivation, &self.pool).await
    }

    pub async fn get_buildable_drvs(&self) -> anyhow::Result<Vec<DrvId>> {
        drv::get_buildable_and_retry_drvs(&self.pool).await
    }

    pub async fn create_github_jobset_with_jobs(
        &self,
        sha: &str,
        name: &str,
        owner: &str,
        repo_name: &str,
        jobs: &[NixEvalDrv],
    ) -> anyhow::Result<i64> {
        let jobset_id = github::create_jobset(sha, name, owner, repo_name, &self.pool).await?;
        github::create_jobs_for_jobset(jobset_id, jobs, &self.pool).await?;
        Ok(jobset_id)
    }

    // Given a head and base sha, determine what has changed
    pub async fn job_difference(
        &self,
        head_sha: &str,
        base_sha: &str,
        job_name: &str,
    ) -> anyhow::Result<(Vec<Drv>, Vec<Drv>, Vec<String>)> {
        github::job_difference(head_sha, base_sha, job_name, &self.pool).await
    }

    pub async fn check_runs_for_drv_path(
        &self,
        drv_path: &DrvId,
    ) -> anyhow::Result<Vec<github::CheckRun>> {
        github::check_runs_for_drv_path(drv_path, &self.pool).await
    }

    pub async fn check_runs_for_commit(&self, sha: &str) -> anyhow::Result<Vec<github::CheckRun>> {
        github::check_runs_for_commit(sha, &self.pool).await
    }

    pub async fn insert_check_run_info(
        &self,
        check_run_id: i64,
        drv_path: &DrvId,
        repo_name: &str,
        repo_owner: &str,
    ) -> anyhow::Result<()> {
        github::insert_check_run_info(check_run_id, drv_path, repo_name, repo_owner, &self.pool)
            .await
    }

    pub async fn has_jobset(
        &self,
        sha: &str,
        name: &str,
        owner: &str,
        repo_name: &str,
    ) -> anyhow::Result<bool> {
        github::has_jobset(sha, name, owner, repo_name, &self.pool).await
    }

    pub async fn update_job_differences(
        &self,
        jobset_id: i64,
        new_drv_ids: &[DrvId],
        changed_drv_ids: &[DrvId],
    ) -> anyhow::Result<()> {
        github::update_job_differences(jobset_id, new_drv_ids, changed_drv_ids, &self.pool).await
    }

    pub async fn get_job_info_for_drv(
        &self,
        drv_id: &DrvId,
    ) -> anyhow::Result<Vec<github::JobInfo>> {
        github::get_job_info_for_drv(drv_id, &self.pool).await
    }

    pub async fn all_jobs_concluded(&self, jobset_id: i64) -> anyhow::Result<bool> {
        github::all_jobs_concluded(jobset_id, &self.pool).await
    }

    pub async fn jobset_has_new_or_changed_failures(&self, jobset_id: i64) -> anyhow::Result<bool> {
        github::jobset_has_new_or_changed_failures(jobset_id, &self.pool).await
    }

    pub async fn get_jobset_info(&self, jobset_id: i64) -> anyhow::Result<github::JobSetInfo> {
        github::get_jobset_info(jobset_id, &self.pool).await
    }

    // Approved users methods
    pub async fn is_user_approved(&self, username: &str, user_id: i64) -> anyhow::Result<bool> {
        approved_users::is_user_approved(username, user_id, &self.pool).await
    }

    pub async fn add_approved_user(
        &self,
        username: &str,
        user_id: i64,
        notes: Option<&str>,
    ) -> anyhow::Result<()> {
        approved_users::add_approved_user(username, user_id, notes, &self.pool).await
    }

    pub async fn remove_approved_user(&self, username: &str) -> anyhow::Result<()> {
        approved_users::remove_approved_user(username, &self.pool).await
    }

    pub async fn list_approved_users(&self) -> anyhow::Result<Vec<approved_users::ApprovedUser>> {
        approved_users::list_approved_users(&self.pool).await
    }

    // GitHub installations methods
    pub async fn upsert_github_installation(
        &self,
        installation: &octocrab::models::Installation,
    ) -> anyhow::Result<()> {
        github_installations::upsert_installation(installation, &self.pool).await
    }

    pub async fn suspend_github_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        github_installations::suspend_installation(installation_id, &self.pool).await
    }

    pub async fn unsuspend_github_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        github_installations::unsuspend_installation(installation_id, &self.pool).await
    }

    pub async fn delete_github_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        github_installations::delete_installation(installation_id, &self.pool).await
    }

    pub async fn get_github_installation_by_id(
        &self,
        installation_id: i64,
    ) -> anyhow::Result<Option<github_installations::GitHubInstallation>> {
        github_installations::get_installation_by_id(installation_id, &self.pool).await
    }

    pub async fn get_github_installation_by_login(
        &self,
        account_login: &str,
    ) -> anyhow::Result<Option<github_installations::GitHubInstallation>> {
        github_installations::get_installation_by_login(account_login, &self.pool).await
    }

    pub async fn list_all_github_installations(
        &self,
    ) -> anyhow::Result<Vec<github_installations::GitHubInstallation>> {
        github_installations::list_all_installations(&self.pool).await
    }

    pub async fn list_active_github_installations(
        &self,
    ) -> anyhow::Result<Vec<github_installations::GitHubInstallation>> {
        github_installations::list_active_installations(&self.pool).await
    }
}
