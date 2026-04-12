use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::{BuildRequest, Builder, Platform};
use crate::metrics::BuildMetrics;

type SystemName = String;

pub struct PlatformQueue {
    platform: Platform,
    builders: HashMap<SystemName, Builder>,
    /// Used to schedule builds with `preferLocalBuild`
    /// This is done to prevent expensive fetches which get downloaded
    /// on remote machines, then get uploaded to the builder
    local_builder: Option<Builder>,
    /// Dedicated builder for Fixed-Output Derivations (FODs)
    /// FODs are fetches (fetchurl, fetchgit, etc.) that have a known output hash
    fod_builder: Option<Builder>,
    metrics: Arc<BuildMetrics>,
}

impl PlatformQueue {
    pub fn new(platform: Platform, metrics: Arc<BuildMetrics>) -> Self {
        Self {
            platform,
            builders: HashMap::new(),
            local_builder: None,
            fod_builder: None,
            metrics,
        }
    }

    pub async fn add_builder(&mut self, builder: Builder) {
        if !builder.is_available().await {
            warn!(
                "{} was rejected as a builder because it was unable to be reached.",
                builder.builder_name
            );
            return;
        }

        if builder.is_local() {
            self.local_builder = Some(builder);
        } else {
            self.builders.insert(builder.builder_name.clone(), builder);
        }
    }

    pub async fn add_fod_builder(&mut self, builder: Builder) {
        self.fod_builder = Some(builder);
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(1000);

        tokio::spawn(async move {
            self.loop_all_builds(rx).await;
        });

        tx
    }

    pub async fn spawn_builder_loop(
        &self,
        builder_name: &str,
        maybe_builder: Option<Builder>,
    ) -> Option<mpsc::Sender<BuildRequest>> {
        if let Some(builder) = maybe_builder {
            let mut builders = HashMap::new();
            builders.insert(builder_name.to_string(), builder);
            return Some(self.spawn_builders_loop(builders).await);
        }

        None
    }

    pub async fn spawn_builders_loop(
        &self,
        builders: HashMap<SystemName, Builder>,
    ) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(100);
        let platform_clone = self.platform.clone();
        let metrics_clone = self.metrics.clone();

        tokio::spawn(async move {
            loop_builds(builders, rx, platform_clone, metrics_clone).await;
        });

        tx
    }

    pub async fn loop_all_builds(mut self, mut receiver: mpsc::Receiver<BuildRequest>) {
        // Extract builder features for orphan detection before moving self
        // We only need the feature sets, not the full builders
        struct BuilderFeatures {
            supported_features: Vec<String>,
            mandatory_features: Vec<String>,
        }

        let mut all_builder_features: Vec<BuilderFeatures> = Vec::new();
        if let Some(ref fod) = self.fod_builder {
            all_builder_features.push(BuilderFeatures {
                supported_features: fod.supported_features.clone(),
                mandatory_features: fod.mandatory_features.clone(),
            });
        }
        if let Some(ref local) = self.local_builder {
            all_builder_features.push(BuilderFeatures {
                supported_features: local.supported_features.clone(),
                mandatory_features: local.mandatory_features.clone(),
            });
        }
        for builder in self.builders.values() {
            all_builder_features.push(BuilderFeatures {
                supported_features: builder.supported_features.clone(),
                mandatory_features: builder.mandatory_features.clone(),
            });
        }

        // Helper function to check if any builder can handle a drv
        // Uses the extracted feature sets
        let can_handle_drv = move |drv: &crate::db::model::Drv| -> bool {
            let required_features: Vec<String> = drv
                .required_system_features
                .as_ref()
                .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                .unwrap_or_default();

            all_builder_features.iter().any(|bf| {
                // Check mandatory features
                if !bf.mandatory_features.is_empty() {
                    let has_mandatory = required_features
                        .iter()
                        .any(|req| bf.mandatory_features.contains(req));
                    if !has_mandatory {
                        return false;
                    }
                }
                // Check if builder has all required features
                required_features
                    .iter()
                    .all(|req| bf.supported_features.contains(req))
            })
        };

        // Get a recorder_sender from any builder (they all share the same one)
        // We need this for orphan detection
        let recorder_sender = if let Some(ref fod) = self.fod_builder {
            Some(fod.recorder_sender().clone())
        } else if let Some(ref local) = self.local_builder {
            Some(local.recorder_sender().clone())
        } else {
            self.builders
                .values()
                .next()
                .map(|b| b.recorder_sender().clone())
        };

        let has_fod = self.fod_builder.is_some();
        let has_local = self.local_builder.is_some();
        let has_remote = !self.builders.is_empty();
        let maybe_fod = self.fod_builder.take();
        let maybe_local = self.local_builder.take();

        let maybe_fod_tx = self.spawn_builder_loop("localhost_fod", maybe_fod).await;
        let maybe_local_tx = self.spawn_builder_loop("localhost", maybe_local).await;

        // Due to mut self, we need to provision this explicitly
        let (remote_tx, remote_rx) = mpsc::channel(100);
        let platform_clone = self.platform.clone();
        let metrics_clone = self.metrics.clone();
        let builders = self.builders;

        tokio::spawn(async move {
            loop_builds(builders, remote_rx, platform_clone, metrics_clone).await;
        });

        // Helper to check for orphaned jobs and fail them immediately
        let check_orphan = |work: &BuildRequest| -> bool {
            let can_build = can_handle_drv(&work.0);

            if !can_build {
                if let Some(ref sender) = recorder_sender {
                    let required_features_str =
                        work.0.required_system_features.clone().unwrap_or_default();
                    warn!(
                        "Job {:?} requires features [{}] that no builder provides. Marking as \
                         UnsatisfiableRequirements.",
                        work.0.drv_path, required_features_str
                    );
                    let task = crate::scheduler::recorder::RecorderTask {
                        derivation: work.0.drv_path.clone(),
                        result:
                            crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements,
                    };
                    // We can't await in a closure, so we spawn a task
                    let sender_clone = sender.clone();
                    tokio::spawn(async move {
                        let _ = sender_clone.send(task).await;
                    });
                }
                return true; // Is orphan
            }
            false // Not orphan
        };

        // Case 1: FOD + local + remote (3-way routing)
        if has_fod && has_local && has_remote {
            let fod_tx = maybe_fod_tx.unwrap();
            let local_tx = maybe_local_tx.unwrap();

            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                if work.0.is_fod {
                    fod_tx
                        .send(work)
                        .await
                        .expect("Failed to send to FOD builder");
                } else if work.0.prefer_local_build {
                    local_tx
                        .send(work)
                        .await
                        .expect("Failed to send to local builder");
                } else {
                    remote_tx
                        .send(work)
                        .await
                        .expect("Failed to send to remote builder pool");
                }
            }
        }
        // Case 2: FOD + local (no remote) - 2-way routing
        else if has_fod && has_local {
            let fod_tx = maybe_fod_tx.unwrap();
            let local_tx = maybe_local_tx.unwrap();

            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                if work.0.is_fod {
                    fod_tx
                        .send(work)
                        .await
                        .expect("Failed to send to FOD builder");
                } else {
                    local_tx
                        .send(work)
                        .await
                        .expect("Failed to send to local builder");
                }
            }
        }
        // Case 3: FOD + remote (no local) - 2-way routing
        else if has_fod && has_remote {
            let fod_tx = maybe_fod_tx.unwrap();

            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                if work.0.is_fod {
                    fod_tx
                        .send(work)
                        .await
                        .expect("Failed to send to FOD builder");
                } else {
                    remote_tx
                        .send(work)
                        .await
                        .expect("Failed to send to remote builder pool");
                }
            }
        }
        // Case 4: local + remote (existing 2-way routing, no FOD)
        else if has_local && has_remote {
            let local_tx = maybe_local_tx.unwrap();

            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                if work.0.prefer_local_build {
                    local_tx
                        .send(work)
                        .await
                        .expect("Failed to send to local builder");
                } else {
                    remote_tx
                        .send(work)
                        .await
                        .expect("Failed to send to remote builder pool");
                }
            }
        }
        // Case 5: Remote only
        else if has_remote {
            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                remote_tx
                    .send(work)
                    .await
                    .expect("Failed to send to remote builder pool");
            }
        }
        // Case 6: Local only
        else {
            let local_tx = maybe_local_tx.expect("Failed to setup local builder");
            while let Some(work) = receiver.recv().await {
                if check_orphan(&work) {
                    continue;
                }

                local_tx
                    .send(work)
                    .await
                    .expect("Failed to send to remote builder pool");
            }
        }
    }
}

async fn loop_builds(
    builders: HashMap<SystemName, Builder>,
    mut receiver: mpsc::Receiver<BuildRequest>,
    platform: Platform,
    metrics: Arc<BuildMetrics>,
) {
    use std::collections::VecDeque;

    use tokio::sync::mpsc::error::TryRecvError;

    let mut build_buffer: VecDeque<BuildRequest> = VecDeque::new();

    // Keep both the builder info and channels for feature filtering
    struct BuilderChannel {
        supported_features: Vec<String>,
        mandatory_features: Vec<String>,
        channel: mpsc::Sender<BuildRequest>,
    }

    let build_channels: Vec<BuilderChannel> = builders
        .into_values()
        .map(|builder| {
            let supported_features = builder.supported_features.clone();
            let mandatory_features = builder.mandatory_features.clone();
            let channel = builder.run();
            BuilderChannel {
                supported_features,
                mandatory_features,
                channel,
            }
        })
        .collect();

    let mut permit_timer = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut build_timer = tokio::time::interval(std::time::Duration::from_millis(100));

    // TODO: This should be restructured to avoid starvation if builds cannot be submitted
    loop {
        match receiver.try_recv() {
            Ok(work) => {
                // Drain channel to ensure there's no back pressure
                build_buffer.push_back(work);
                // Update queued builds metric
                metrics
                    .queued_builds
                    .with_label_values(&[&platform])
                    .set(build_buffer.len() as f64);
                continue;
            },
            Err(TryRecvError::Disconnected) => {
                warn!("System queue closing due to disconnected build queue");
                return;
            },
            // No one has submitted work, just fall through
            Err(TryRecvError::Empty) => {},
        }

        if !build_buffer.is_empty() {
            let current_job = build_buffer.front().unwrap();

            // Parse required features for current job
            let required_features: Vec<String> = current_job
                .0
                .required_system_features
                .as_ref()
                .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                .unwrap_or_default();

            // Filter builders that can handle this job
            let compatible_builders: Vec<&BuilderChannel> = build_channels
                .iter()
                .filter(|bc| {
                    // Check mandatory features
                    if !bc.mandatory_features.is_empty() {
                        let has_mandatory = required_features
                            .iter()
                            .any(|req| bc.mandatory_features.contains(req));
                        if !has_mandatory {
                            return false;
                        }
                    }
                    // Check if builder has all required features
                    required_features
                        .iter()
                        .all(|req| bc.supported_features.contains(req))
                })
                .collect();

            // Do a scan of compatible builders to see if they have capacity
            let permit = 'outer: loop {
                for bc in &compatible_builders {
                    if let Ok(permit) = bc.channel.try_reserve() {
                        break 'outer permit;
                    }
                }
                permit_timer.tick().await;
            };

            permit.send(build_buffer.pop_front().unwrap());
            // Update queued builds metric
            metrics
                .queued_builds
                .with_label_values(&[&platform])
                .set(build_buffer.len() as f64);
        } else {
            build_timer.tick().await;
        }
    }
}

