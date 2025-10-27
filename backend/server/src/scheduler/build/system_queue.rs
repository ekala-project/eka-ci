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
    build_receiver: mpsc::Receiver<BuildRequest>,
}

impl PlatformQueue {
    pub fn new(platform: Platform, build_receiver: mpsc::Receiver<BuildRequest>) -> Self {
        Self {
            platform,
            builders: HashMap::new(),
            build_receiver,
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

    pub async fn loop_builds(mut self, receiver: mpsc::Receiver<BuildRequest>) {
        use tokio::task::JoinSet;

        let permits: JoinSet<Permit<BuildRequest>> = JoinSet::new();

        for (_, builder) in self.builders {
            let sender = builder.run();
            permits.spawn(async move {
                get_permit(sender).await
            });
        }

        loop {
            if let Some(Ok(permit)) = permits.join_next().await {
                let work = receiver.recv().await;
                permit.send(work);
            } else {
                error!("{} queue does not have any workers, aborting", self.platform);
                break;
            }
        }
    }
}

async fn get_permit(tx: mpsc::Sender<BuildRequest>) -> Result<(Permit<BuildRequest>, mpsc::Sender<BuildRequest>)> {
    let permit = tx.reserve().await?;
    Ok((permit, tx))
}
