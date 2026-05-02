// State management and broadcasting for build events

use chrono::Utc;

use super::RecorderWorker;
use crate::db::model::{build_event, drv_id};
use crate::graph::GraphCommand;
use crate::services::websocket::events::{BuildStateChange, JobStatsUpdate, ServerEvent};

impl RecorderWorker {
    /// Broadcast a build state change event to WebSocket clients
    pub(super) fn broadcast_state_change(
        &self,
        drv: &drv_id::DrvId,
        old_state: &build_event::DrvBuildState,
        new_state: &build_event::DrvBuildState,
    ) {
        if let Some(ref sender) = self.websocket_sender {
            let event = ServerEvent::BuildStateChange(BuildStateChange {
                drv_path: drv.store_path().to_string(),
                old_state: old_state.clone(),
                new_state: new_state.clone(),
                timestamp: Utc::now(),
            });

            // Broadcast the event. `broadcast::Sender::send` returns
            // `Err(SendError)` iff there are no active receivers — a
            // normal operational state when no WebSocket clients are
            // connected — so the drop here is deliberate.
            let _no_receivers = sender.send(event);
        }
    }

    /// Broadcast job statistics update for a specific job
    pub(super) async fn broadcast_job_stats(&self, jobset_id: i64) {
        if let Some(ref sender) = self.websocket_sender {
            // Fetch current job stats from database
            if let Ok(details) = self.db_service.get_jobset_details(jobset_id).await {
                let event = ServerEvent::JobStatsUpdate(JobStatsUpdate {
                    jobset_id,
                    total_drvs: details.total_drvs,
                    queued_drvs: details.queued_drvs,
                    buildable_drvs: details.buildable_drvs,
                    building_drvs: details.building_drvs,
                    completed_success_drvs: details.completed_success_drvs,
                    completed_failure_drvs: details.completed_failure_drvs,
                    failed_retry_drvs: details.failed_retry_drvs,
                    transitive_failure_drvs: details.transitive_failure_drvs,
                    blocked_drvs: details.blocked_drvs,
                    interrupted_drvs: details.interrupted_drvs,
                    timestamp: Utc::now(),
                });

                // Broadcast the event. Err iff no active WebSocket
                // subscribers — a normal operational state.
                let _no_receivers = sender.send(event);
            }
        }
    }

    /// Update drv status in both graph and database, then broadcast the change
    pub(super) async fn update_and_broadcast(
        &self,
        drv: &drv_id::DrvId,
        old_state: &build_event::DrvBuildState,
        new_state: &build_event::DrvBuildState,
    ) -> anyhow::Result<()> {
        // Update graph first (fast in-memory operation)
        self.update_graph_state(drv, new_state.clone()).await?;

        // Then update database (for persistence)
        self.db_service.update_drv_status(drv, new_state).await?;

        // Finally broadcast to websocket clients
        self.broadcast_state_change(drv, old_state, new_state);
        Ok(())
    }

    /// Send UpdateState command to graph service
    pub(super) async fn update_graph_state(
        &self,
        drv_id: &drv_id::DrvId,
        new_state: build_event::DrvBuildState,
    ) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::UpdateState {
            drv_id: drv_id.clone(),
            new_state,
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        rx.await?;
        Ok(())
    }

    /// Send ClearFailure command to graph service
    pub(super) async fn clear_graph_failure(
        &self,
        drv_id: &drv_id::DrvId,
    ) -> anyhow::Result<Vec<drv_id::DrvId>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::ClearFailure {
            formerly_failed: drv_id.clone(),
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        let unblocked = rx.await?;
        Ok(unblocked)
    }

    /// Send PropagateFailure command to graph service
    pub(super) async fn propagate_graph_failure(
        &self,
        drv_id: &drv_id::DrvId,
    ) -> anyhow::Result<Vec<drv_id::DrvId>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::PropagateFailure {
            failed_drv: drv_id.clone(),
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        let blocked = rx.await?;
        Ok(blocked)
    }
}
