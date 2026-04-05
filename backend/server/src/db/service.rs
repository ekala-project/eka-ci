use std::path::Path;

use sqlx::migrate;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tracing::{debug, info};

use super::model::drv::Drv;
use super::model::drv_id::DrvId;
use super::model::{build_event, drv};
use super::{approved_users, github, installations};
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

    // Repository management methods
    pub async fn list_repositories(&self) -> anyhow::Result<Vec<github::RepositoryInfo>> {
        github::list_repositories(&self.pool).await
    }

    pub async fn get_repository(
        &self,
        owner: &str,
        repo_name: &str,
    ) -> anyhow::Result<Option<github::RepositoryInfo>> {
        github::get_repository(owner, repo_name, &self.pool).await
    }

    pub async fn list_repository_commits(
        &self,
        owner: &str,
        repo_name: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<github::CommitInfo>> {
        github::list_repository_commits(owner, repo_name, limit, &self.pool).await
    }

    // Job and build status methods
    pub async fn get_commit_jobs(&self, sha: &str) -> anyhow::Result<Vec<github::CommitJob>> {
        github::get_commit_jobs(sha, &self.pool).await
    }

    pub async fn get_jobset_details(
        &self,
        jobset_id: i64,
    ) -> anyhow::Result<github::JobSetDetails> {
        github::get_jobset_details(jobset_id, &self.pool).await
    }

    pub async fn get_jobset_drvs(
        &self,
        jobset_id: i64,
        state_filter: Option<build_event::DrvBuildState>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<github::JobSetDrv>> {
        github::get_jobset_drvs(jobset_id, state_filter, limit, offset, &self.pool).await
    }

    pub async fn count_jobset_drvs(&self, jobset_id: i64) -> anyhow::Result<i64> {
        github::count_jobset_drvs(jobset_id, &self.pool).await
    }

    pub async fn get_active_jobs(&self) -> anyhow::Result<Vec<github::JobSetDetails>> {
        github::get_active_jobs(&self.pool).await
    }

    pub async fn get_all_building_drvs(&self) -> anyhow::Result<Vec<github::BuildingDrv>> {
        github::get_all_building_drvs(&self.pool).await
    }

    // Derivation details methods
    pub async fn get_drv_details(&self, drv_id: &DrvId) -> anyhow::Result<Option<drv::DrvDetails>> {
        drv::get_drv_details(drv_id, &self.pool).await
    }

    pub async fn get_drv_dependencies(
        &self,
        drv_id: &DrvId,
    ) -> anyhow::Result<Vec<drv::DrvDependency>> {
        drv::get_drv_dependencies(drv_id, &self.pool).await
    }

    pub async fn count_drv_dependencies(&self, drv_id: &DrvId) -> anyhow::Result<i64> {
        drv::count_drv_dependencies(drv_id, &self.pool).await
    }

    // GitHub installation methods
    pub async fn upsert_installation(
        &self,
        installation_id: i64,
        account_type: &str,
        account_login: &str,
    ) -> anyhow::Result<()> {
        installations::upsert_installation(installation_id, account_type, account_login, &self.pool)
            .await
    }

    pub async fn suspend_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        installations::suspend_installation(installation_id, &self.pool).await
    }

    pub async fn unsuspend_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        installations::unsuspend_installation(installation_id, &self.pool).await
    }

    pub async fn delete_installation(&self, installation_id: i64) -> anyhow::Result<()> {
        installations::delete_installation(installation_id, &self.pool).await
    }

    pub async fn upsert_installation_repository(
        &self,
        installation_id: i64,
        repo_id: i64,
        repo_name: &str,
        repo_owner: &str,
    ) -> anyhow::Result<()> {
        installations::upsert_installation_repository(
            installation_id,
            repo_id,
            repo_name,
            repo_owner,
            &self.pool,
        )
        .await
    }

    pub async fn delete_installation_repository(
        &self,
        installation_id: i64,
        repo_id: i64,
    ) -> anyhow::Result<()> {
        installations::delete_installation_repository(installation_id, repo_id, &self.pool).await
    }

    pub async fn is_repository_installed(
        &self,
        owner: &str,
        repo_name: &str,
    ) -> anyhow::Result<bool> {
        installations::is_repository_installed(owner, repo_name, &self.pool).await
    }

    pub async fn list_installed_repositories(
        &self,
    ) -> anyhow::Result<Vec<installations::InstallationRepository>> {
        installations::list_installed_repositories(&self.pool).await
    }

    pub async fn get_installation_repositories(
        &self,
        installation_id: i64,
    ) -> anyhow::Result<Vec<installations::InstallationRepository>> {
        installations::get_installation_repositories(installation_id, &self.pool).await
    }

    pub async fn get_installation(
        &self,
        installation_id: i64,
    ) -> anyhow::Result<Option<installations::GitHubInstallation>> {
        installations::get_installation(installation_id, &self.pool).await
    }
}
