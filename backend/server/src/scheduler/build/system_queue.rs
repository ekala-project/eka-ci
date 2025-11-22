use std::collections::HashMap;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::{BuildRequest, Builder, Platform};

type SystemName = String;

pub struct PlatformQueue {
    platform: Platform,
    builders: HashMap<SystemName, Builder>,
    /// Used to schedule builds with `preferLocalBuild`
    /// This is done to prevent expensive fetches which get downloaded
    /// on remote machines, then get uploaded to the builder
    local_builder: Option<Builder>,
}

impl PlatformQueue {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            builders: HashMap::new(),
            local_builder: None,
        }
    }

    pub fn add_builder(&mut self, builder: Builder) {
        if builder.is_local() {
            self.local_builder = Some(builder);
        } else {
            self.builders.insert(builder.builder_name.clone(), builder);
        }
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(1000);

        tokio::spawn(async move {
            self.loop_all_builds(rx).await;
        });

        tx
    }

    pub async fn loop_all_builds(mut self, mut receiver: mpsc::Receiver<BuildRequest>) {
        // If we have both local and remote builders, try to separate the concerns.
        if self.local_builder.is_some() && !self.builders.is_empty() {
            let local_builder = self.local_builder.unwrap();
            // TODO: buffer this so that one doesn't pause the other
            let (remote_tx, remote_rx) = mpsc::channel(1000);
            let (local_tx, local_rx) = mpsc::channel(1000);
            let mut local_builders = HashMap::new();
            local_builders.insert("localhost".to_string(), local_builder);

            debug!("{} remote system queue started", &self.platform);
            tokio::spawn(async move {
                loop_builds(self.builders, remote_rx).await;
            });
            debug!("{} local system queue started", &self.platform);
            tokio::spawn(async move {
                loop_builds(local_builders, local_rx).await;
            });

            while let Some(work) = receiver.recv().await {
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
        } else {
            // Collapse local and remote as we have one or the other. But not both
            if let Some(local_builder) = self.local_builder {
                self.builders.insert("localhost".to_string(), local_builder);
            }
            debug!("{} system queue started", &self.platform);
            loop_builds(self.builders, receiver).await;
        }
    }
}

async fn loop_builds(
    builders: HashMap<SystemName, Builder>,
    mut receiver: mpsc::Receiver<BuildRequest>,
) {
    use std::collections::VecDeque;
    use tokio::sync::mpsc::error::TryRecvError;

    let mut build_buffer: VecDeque::<BuildRequest> = VecDeque::new();
    let build_channels: Vec<mpsc::Sender<BuildRequest>> =
        builders.into_values().map(|x| x.run()).collect();
    let mut permit_timer = tokio::time::interval(std::time::Duration::from_millis(10));
    let mut build_timer = tokio::time::interval(std::time::Duration::from_millis(100));

    // TODO: This should be restructured to avoid starvation if builds cannot be submitted
    loop {
        match receiver.try_recv() {
            Ok(work) => {
                // Drain channel to ensure there's no back pressure
                build_buffer.push_back(work);
                continue;
            },
            Err(TryRecvError::Disconnected) => {
                warn!("System queue closing due to disconnected build queue");
                return;
            },
            // No one has submitted work, just fall through
            Err(TryRecvError::Empty) => { }
        }

        if build_buffer.len() > 0 {
            // Do a scan of the builder to see if they have capacity. Otherwise wait
            // TODO: think of way to poll many builders for availability without doing scan
            let permit = 'outer: loop {
                for channel in &build_channels {
                    if let Ok(permit) = channel.try_reserve() {
                        break 'outer permit;
                    }
                }
                permit_timer.tick().await;
            };

            permit.send(build_buffer.pop_front().unwrap());
        } else {
            build_timer.tick().await;
        }
    }
}
