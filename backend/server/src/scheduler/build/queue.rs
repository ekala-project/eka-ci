use std::collections::HashMap;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error};

use super::{Builder, Platform, PlatformQueue};
use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::scheduler::recorder::RecorderTask;

#[derive(Debug)]
pub struct BuildRequest(pub Drv);

/// This acts as the service which monitors a "nix build" and reports the
/// status of a build
///
/// TODO: Allow for number of parallel builds to be configured
///       tokio::task::JoinSet would likely be a good option for this
pub struct BuildQueue {
    db_service: DbService,
    build_request_receiver: mpsc::Receiver<BuildRequest>,
    system_queues: HashMap<Platform, PlatformQueue>,
}

impl BuildQueue {
    /// Immediately starts builder service
    pub fn init(
        db_service: DbService,
        builders: Vec<Builder>,
    ) -> (Self, mpsc::Sender<BuildRequest>) {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);
        let system_queues = HashMap::new();

        let mut queue = Self {
            db_service,
            build_request_receiver,
            system_queues,
        };

        for builder in builders {
            queue.add_builder(builder);
        }

        (queue, build_request_sender)
    }

    fn add_builder(&mut self, builder: Builder) {
        if !self.system_queues.contains_key(&builder.platform) {
            let queue = PlatformQueue::new(builder.platform.clone());
            self.system_queues.insert(builder.platform.clone(), queue);
        }

        let system_queue = self.system_queues.get_mut(&builder.platform).unwrap();
        system_queue.add_builder(builder);
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
        let default_platform = default_platform().await;

        loop {
            if let Some(build_request) = self.build_request_receiver.recv().await {
                debug!("Recieved request for build: {:?}", &build_request);

                // 'system = "builtin";' is for <nix/fetchurl> and a few others which are the first
                // FOD to be made for a drv graph
                let platform = if &build_request.0.system == "builtin" {
                    &default_platform
                } else {
                    &build_request.0.system
                };

                debug!("Attempting to get queue for {}", platform);
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

async fn default_platform() -> Platform {
    let output = Command::new("nix-instantiate")
        .args(["--eval", "--raw", "-E", "builtins.currentSystem"])
        .output()
        .await
        .expect("failed to run nix-instantiate")
        .stdout;
    let platform = String::from_utf8(output).expect("Invalid string from nix-instantiate");
    debug!("Using {} as default platform", &platform);
    platform
}
