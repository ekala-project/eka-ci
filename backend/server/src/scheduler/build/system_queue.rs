use std::collections::HashMap;

use tokio::sync::mpsc;
use tracing::debug;

use super::{BuildRequest, Builder, Platform};

type SystemName = String;

pub struct PlatformQueue {
    platform: Platform,
    builders: HashMap<SystemName, Builder>,
}

impl PlatformQueue {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            builders: HashMap::new(),
        }
    }

    pub fn add_builder(&mut self, builder: Builder) {
        self.builders.insert(builder.builder_name.clone(), builder);
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(1000);

        tokio::spawn(async move {
            self.loop_builds(rx).await;
        });

        tx
    }

    pub async fn loop_builds(self, mut receiver: mpsc::Receiver<BuildRequest>) {
        let build_channels: Vec<mpsc::Sender<BuildRequest>> =
            self.builders.into_values().map(|x| x.run()).collect();

        debug!("{} system queue started", &self.platform);
        while let Some(work) = receiver.recv().await {
            debug!(
                "{} system queue received build request: {:?}",
                &self.platform, &work
            );
            for channel in &build_channels {
                if let Ok(permit) = channel.try_reserve() {
                    permit.send(work);
                    break;
                }
            }
        }
    }
}
