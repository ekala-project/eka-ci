use std::collections::HashMap;
use super::{Builder, BuildRequest, Platform};
use tokio::sync::mpsc;
use anyhow::Result;
use tokio::sync::mpsc::Permit;
use tracing::error;

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

    pub async fn loop_builds<'a>(mut self, mut receiver: mpsc::Receiver<BuildRequest>) {
        let build_channels: Vec<mpsc::Sender<BuildRequest>> = self.builders.into_iter().map(|(_, x)| x.run()).collect();
        while let Some(work) = receiver.recv().await {
            for channel in &build_channels {
                if let Ok(permit) = channel.try_reserve() {
                    permit.send(work);
                    break;
                }
            }
        }
    }
}
