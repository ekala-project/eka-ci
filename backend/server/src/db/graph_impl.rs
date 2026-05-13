// Implementation of graph::traits::GraphDatabase for DbService

use async_trait::async_trait;
use graph::traits::GraphDatabase;

use super::DbService;

#[async_trait]
impl GraphDatabase for DbService {
    async fn get_drv(
        &self,
        drv_path: &shared::types::DrvId,
    ) -> anyhow::Result<Option<shared::types::Drv>> {
        // Convert shared::types::DrvId to db::model::DrvId
        let local_id = drv_path.to_string().parse()?;

        // Call the existing method
        let local_drv = self.get_drv(&local_id).await?;

        // Convert db::model::Drv to shared::types::Drv
        Ok(local_drv.map(|d| shared::types::Drv {
            drv_path: d.drv_path.to_string().parse().unwrap(),
            system: d.system,
            prefer_local_build: d.prefer_local_build,
            required_system_features: d.required_system_features,
            is_fod: d.is_fod,
            build_state: convert_build_state(&d.build_state),
            output_size: d.output_size,
            closure_size: d.closure_size,
            pname: d.pname,
            version: d.version,
            license_json: d.license_json,
            maintainers_json: d.maintainers_json,
            meta_position: d.meta_position,
            broken: d.broken,
            insecure: d.insecure,
        }))
    }

    async fn update_drv_status(
        &self,
        drv_id: &shared::types::DrvId,
        state: &shared::types::DrvBuildState,
    ) -> anyhow::Result<()> {
        let local_id = drv_id.to_string().parse()?;
        let local_state = convert_build_state_back(state);
        self.update_drv_status(&local_id, &local_state).await
    }

    async fn get_all_drvs(&self) -> anyhow::Result<Vec<shared::types::Drv>> {
        let local_drvs = self.get_all_drvs().await?;
        Ok(local_drvs
            .into_iter()
            .map(|d| shared::types::Drv {
                drv_path: d.drv_path.to_string().parse().unwrap(),
                system: d.system,
                prefer_local_build: d.prefer_local_build,
                required_system_features: d.required_system_features,
                is_fod: d.is_fod,
                build_state: convert_build_state(&d.build_state),
                output_size: d.output_size,
                closure_size: d.closure_size,
                pname: d.pname,
                version: d.version,
                license_json: d.license_json,
                maintainers_json: d.maintainers_json,
                meta_position: d.meta_position,
                broken: d.broken,
                insecure: d.insecure,
            })
            .collect())
    }

    async fn get_all_refs(&self) -> anyhow::Result<Vec<(shared::types::DrvId, shared::types::DrvId)>> {
        let local_refs = self.get_all_refs().await?;
        let converted: Result<Vec<_>, _> = local_refs
            .into_iter()
            .map(|(r, d)| {
                Ok((
                    r.to_string().parse()?,
                    d.to_string().parse()?,
                ))
            })
            .collect();
        converted
    }

    async fn get_drv_refs(&self, drv_id: &shared::types::DrvId) -> anyhow::Result<Vec<shared::types::DrvId>> {
        let local_id = drv_id.to_string().parse()?;
        let local_refs = self.get_drv_refs(&local_id).await?;
        let converted: Result<Vec<_>, _> = local_refs
            .into_iter()
            .map(|id| id.to_string().parse().map_err(anyhow::Error::from))
            .collect();
        converted
    }

    async fn get_drv_dependents(
        &self,
        drv_id: &shared::types::DrvId,
    ) -> anyhow::Result<Vec<shared::types::DrvId>> {
        let local_id = drv_id.to_string().parse()?;
        let local_deps = self.get_drv_dependents(&local_id).await?;
        let converted: Result<Vec<_>, _> = local_deps
            .into_iter()
            .map(|id| id.to_string().parse().map_err(anyhow::Error::from))
            .collect();
        converted
    }

    async fn insert_transitive_failures(
        &self,
        failed_drv: &shared::types::DrvId,
        transitive_referrers: &[shared::types::DrvId],
    ) -> anyhow::Result<()> {
        let local_failed = failed_drv.to_string().parse()?;
        let local_referrers: Result<Vec<_>, _> = transitive_referrers
            .iter()
            .map(|id| id.to_string().parse())
            .collect();
        self.insert_transitive_failures(&local_failed, &local_referrers?).await
    }

    async fn clear_transitive_failures(
        &self,
        drv: &shared::types::DrvId,
    ) -> anyhow::Result<Vec<shared::types::DrvId>> {
        let local_id = drv.to_string().parse()?;
        let local_unblocked = self.clear_transitive_failures(&local_id).await?;
        let converted: Result<Vec<_>, _> = local_unblocked
            .into_iter()
            .map(|id| id.to_string().parse().map_err(anyhow::Error::from))
            .collect();
        converted
    }
}

// Helper functions to convert between build state types
fn convert_build_state(state: &crate::db::model::build_event::DrvBuildState) -> shared::types::DrvBuildState {
    use crate::db::model::build_event::{DrvBuildInterruptionKind as LocalInterrupt, DrvBuildResult as LocalResult, DrvBuildState as LocalState};
    use shared::types::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    match state {
        LocalState::Queued => DrvBuildState::Queued,
        LocalState::Buildable => DrvBuildState::Buildable,
        LocalState::FailedRetry => DrvBuildState::FailedRetry,
        LocalState::Building => DrvBuildState::Building,
        LocalState::Completed(LocalResult::Success) => DrvBuildState::Completed(DrvBuildResult::Success),
        LocalState::Completed(LocalResult::Failure) => DrvBuildState::Completed(DrvBuildResult::Failure),
        LocalState::Interrupted(LocalInterrupt::Cancelled) => DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled),
        LocalState::Interrupted(LocalInterrupt::Timeout) => DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout),
        LocalState::Interrupted(LocalInterrupt::OutOfMemory) => DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory),
        LocalState::Interrupted(LocalInterrupt::ProcessDeath) => DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath),
        LocalState::Interrupted(LocalInterrupt::SchedulerDeath) => DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath),
        LocalState::TransitiveFailure => DrvBuildState::TransitiveFailure,
        LocalState::Blocked => DrvBuildState::Blocked,
        LocalState::UnsatisfiableRequirements => DrvBuildState::UnsatisfiableRequirements,
    }
}

fn convert_build_state_back(state: &shared::types::DrvBuildState) -> crate::db::model::build_event::DrvBuildState {
    use crate::db::model::build_event::{DrvBuildInterruptionKind as LocalInterrupt, DrvBuildResult as LocalResult, DrvBuildState as LocalState};
    use shared::types::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    match state {
        DrvBuildState::Queued => LocalState::Queued,
        DrvBuildState::Buildable => LocalState::Buildable,
        DrvBuildState::FailedRetry => LocalState::FailedRetry,
        DrvBuildState::Building => LocalState::Building,
        DrvBuildState::Completed(DrvBuildResult::Success) => LocalState::Completed(LocalResult::Success),
        DrvBuildState::Completed(DrvBuildResult::Failure) => LocalState::Completed(LocalResult::Failure),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => LocalState::Interrupted(LocalInterrupt::Cancelled),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => LocalState::Interrupted(LocalInterrupt::Timeout),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => LocalState::Interrupted(LocalInterrupt::OutOfMemory),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => LocalState::Interrupted(LocalInterrupt::ProcessDeath),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => LocalState::Interrupted(LocalInterrupt::SchedulerDeath),
        DrvBuildState::TransitiveFailure => LocalState::TransitiveFailure,
        DrvBuildState::Blocked => LocalState::Blocked,
        DrvBuildState::UnsatisfiableRequirements => LocalState::UnsatisfiableRequirements,
    }
}
