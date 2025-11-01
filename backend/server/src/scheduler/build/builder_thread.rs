use std::process::Output;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

use super::BuildRequest;
use crate::db::model::{DrvId, build_event};
use crate::scheduler::recorder::RecorderTask;

pub struct BuilderThread {
    build_args: [String; 2],
    max_jobs: u8,
    recorder_sender: mpsc::Sender<RecorderTask>,
}

impl BuilderThread {
    pub fn init(
        build_args: [String; 2],
        max_jobs: u8,
        recorder_sender: mpsc::Sender<RecorderTask>,
    ) -> Self {
        Self {
            build_args,
            max_jobs,
            recorder_sender,
        }
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(self.max_jobs.into());

        tokio::spawn(async move {
            self.loop_for_builds(rx).await;
        });

        tx
    }

    async fn loop_for_builds(self, mut build_receiver: mpsc::Receiver<BuildRequest>) {
        use std::time::Duration;

        let mut interval = tokio::time::interval(Duration::from_millis(1));
        let mut build_set = JoinSet::new();

        loop {
            if build_set.len() >= self.max_jobs.into() {
                match build_set.join_next().await {
                    Some(Err(e)) => warn!("Failed to execute nix build, {:?}", e),
                    None => error!("Tried to await empty build queue"),
                    _ => debug!("Successfully built a drv"),
                }
            }

            if let Some(build_request) = build_receiver.recv().await {
                let new_build = self.create_build(build_request.0.drv_path);
                build_set.spawn(async move { new_build.attempt_build().await });
            } else {
                interval.tick().await;
            }
        }
    }

    fn create_build(&self, drv_id: DrvId) -> NixBuild {
        NixBuild {
            build_args: self.build_args.clone(),
            recorder_sender: self.recorder_sender.clone(),
            drv_id,
        }
    }
}

struct NixBuild {
    build_args: [String; 2],
    recorder_sender: mpsc::Sender<RecorderTask>,
    drv_id: DrvId,
}

impl NixBuild {
    async fn perform_build(&self) -> build_event::DrvBuildState {
        use build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

        let drv_path = self.drv_id.store_path();
        match self.build_drv().await {
            Ok(output) => {
                if output.status.success() {
                    debug!("Successfully built {:?}", drv_path);
                    DrvBuildState::Completed(DrvBuildResult::Success)
                } else {
                    debug!("Build failed for {:?}", drv_path);
                    DrvBuildState::Completed(DrvBuildResult::Failure)
                }
            },

            // Err doesn't denote process failure, rather process construction
            Err(e) => {
                warn!("Failed to build {:?}, encountered error: {:?}", drv_path, e);
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath)
            },
        }
    }

    async fn build_drv(&self) -> anyhow::Result<Output> {
        debug!("Building {} drv", self.drv_id.store_path());
        let build_output = Command::new("nix-build")
            .args([self.drv_id.store_path()])
            .args(&self.build_args)
            .output()
            .await?;

        Ok(build_output)
    }

    async fn attempt_build(self) -> anyhow::Result<()> {
        // use build_event::{DrvBuildEvent, DrvBuildState};
        // let build_id = DrvBuildId {
        //     derivation: drv.drv_path.clone(),
        //     // TODO: build_attempt seems like something we should query
        //     build_attempt: std::num::NonZeroU32::new(1).unwrap(),
        // };

        // let buildable_event = DrvBuildEvent::for_insert(build_id, DrvBuildState::Building);
        // db_service.new_drv_build_event(buildable_event).await?;

        let build_state = self.perform_build().await;

        // To avoid the state of the build not pushing the result to other potential
        // drvs, we let the recorder deal with updating the build_event task
        // and determining if other drv's now can be queued
        let recorder_task = RecorderTask {
            derivation: self.drv_id,
            result: build_state.clone(),
        };

        self.recorder_sender.send(recorder_task).await?;

        Ok(())
    }
}
