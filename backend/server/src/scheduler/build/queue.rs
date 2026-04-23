use std::collections::HashMap;
use std::sync::Arc;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

use super::{Builder, Platform, PlatformQueue};
use crate::db::model::drv::Drv;
use crate::metrics::BuildMetrics;

#[derive(Debug)]
pub struct BuildRequest(pub Drv);

/// This acts as the service which monitors a "nix build" and reports the
/// status of a build
///
/// TODO: Allow for number of parallel builds to be configured
///       tokio::task::JoinSet would likely be a good option for this
pub struct BuildQueue {
    build_request_receiver: mpsc::Receiver<BuildRequest>,
    system_queues: HashMap<Platform, PlatformQueue>,
}

impl BuildQueue {
    /// Immediately starts builder service
    pub async fn init(
        builders: Vec<Builder>,
        fod_builders: Vec<Builder>,
        metrics: Arc<BuildMetrics>,
    ) -> (Self, mpsc::Sender<BuildRequest>) {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);
        let system_queues = HashMap::new();

        let mut queue = Self {
            build_request_receiver,
            system_queues,
        };

        for builder in builders {
            queue.add_builder(builder, metrics.clone()).await;
        }

        for fod_builder in fod_builders {
            queue.add_fod_builder(fod_builder, metrics.clone()).await;
        }

        (queue, build_request_sender)
    }

    async fn add_builder(&mut self, builder: Builder, metrics: Arc<BuildMetrics>) {
        if !self.system_queues.contains_key(&builder.platform) {
            let queue = PlatformQueue::new(builder.platform.clone(), metrics.clone());
            self.system_queues.insert(builder.platform.clone(), queue);
        }

        let system_queue = self.system_queues.get_mut(&builder.platform).unwrap();
        system_queue.add_builder(builder).await;
    }

    async fn add_fod_builder(&mut self, builder: Builder, metrics: Arc<BuildMetrics>) {
        if !self.system_queues.contains_key(&builder.platform) {
            let queue = PlatformQueue::new(builder.platform.clone(), metrics.clone());
            self.system_queues.insert(builder.platform.clone(), queue);
        }

        let system_queue = self.system_queues.get_mut(&builder.platform).unwrap();
        system_queue.add_fod_builder(builder).await;
    }

    pub fn run(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.poll_for_builds().await;
        })
    }

    async fn poll_for_builds(mut self) {
        let mut system_senders: HashMap<Platform, mpsc::Sender<BuildRequest>> = self
            .system_queues
            .into_iter()
            .map(|(system, queue)| (system, queue.run()))
            .collect();
        let default_platform = match default_platform().await {
            Ok(p) => Some(p),
            Err(e) => {
                warn!(
                    "Failed to resolve default platform via nix-instantiate (`builtin` builds \
                     will be rejected): {:?}",
                    e
                );
                None
            },
        };

        loop {
            if let Some(build_request) = self.build_request_receiver.recv().await {
                debug!("Recieved request for build: {:?}", &build_request);

                // 'system = "builtin";' is for <nix/fetchurl> and a few others which are the first
                // FOD to be made for a drv graph
                let platform = if &build_request.0.system == "builtin" {
                    match default_platform.as_ref() {
                        Some(p) => p,
                        None => {
                            error!(
                                "Cannot route `builtin` build: default platform unknown \
                                 (nix-instantiate unavailable)"
                            );
                            continue;
                        },
                    }
                } else {
                    &build_request.0.system
                };

                if let Some(sender) = system_senders.get_mut(platform) {
                    if let Err(e) = sender.send(build_request).await {
                        error!("Failed to send build request: {:?}", e);
                    }
                } else {
                    error!("Failed to get system queue");
                }
            }
        }
    }
}

async fn default_platform() -> anyhow::Result<Platform> {
    use anyhow::Context;

    let output = Command::new("nix-instantiate")
        .args(["--eval", "--raw", "-E", "builtins.currentSystem"])
        .output()
        .await
        .context("failed to run nix-instantiate to determine default platform")?
        .stdout;
    let platform = String::from_utf8(output)
        .context("nix-instantiate emitted non-UTF-8 output for default platform")?;
    debug!("Using {} as default platform", &platform);
    Ok(platform)
}
