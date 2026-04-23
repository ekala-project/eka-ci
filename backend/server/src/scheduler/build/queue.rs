use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
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

    /// M9: The build queue is an *accumulating* queue. We never block
    /// on `system_sender.send().await` and we never drop build
    /// requests, because both would ripple backpressure all the way
    /// back to the webhook ingress task and either stall HTTP workers
    /// or lose builds.
    ///
    /// Shape:
    ///
    /// 1. Drain every message currently waiting on `build_request_receiver` into per-platform
    ///    `VecDeque`s (`pending`). The `try_recv` loop never awaits, so upstream producers
    ///    (scheduler ingress, and further upstream the webhook handler) only see backpressure when
    ///    the 100-slot mpsc filling is fast enough to outpace a tight-loop drain — effectively
    ///    never in practice.
    /// 2. For each platform queue, dispatch items into the corresponding per-platform
    ///    `system_sender` using `try_reserve`. `try_reserve` is non-blocking: on failure we simply
    ///    leave the item at the front of the `VecDeque` and retry on the next tick. Memory grows
    ///    with pending work, which is the explicit trade-off.
    /// 3. If both `pending` is fully empty *and* the receiver is empty, fall into a plain blocking
    ///    `recv().await` so we don't burn CPU on empty cycles. Otherwise sleep a few ms on a
    ///    `tokio::time::interval` so downstream queues can drain.
    async fn poll_for_builds(mut self) {
        let system_senders: HashMap<Platform, mpsc::Sender<BuildRequest>> = self
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

        // Per-platform accumulator. Requests land here regardless of
        // whether the matching `system_sender` currently has capacity;
        // dispatch runs opportunistically on each loop iteration.
        let mut pending: HashMap<Platform, VecDeque<BuildRequest>> = HashMap::new();

        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(10));

        loop {
            // Step 1: drain everything currently on the receiver into
            // the per-platform VecDeques without awaiting.
            loop {
                match self.build_request_receiver.try_recv() {
                    Ok(build_request) => {
                        debug!("Received request for build: {:?}", &build_request);
                        classify_and_enqueue(
                            build_request,
                            default_platform.as_deref(),
                            &mut pending,
                        );
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        warn!(
                            "Build request channel closed; {} pending builds will be abandoned on \
                             shutdown",
                            pending.values().map(|q| q.len()).sum::<usize>()
                        );
                        return;
                    },
                }
            }

            // Step 2: non-blocking dispatch from each platform queue to
            // its per-system builder pool. `try_reserve` keeps items in
            // the VecDeque when the downstream is at capacity so we
            // don't need to re-queue on failure.
            for (platform, queue) in pending.iter_mut() {
                let Some(sender) = system_senders.get(platform) else {
                    // Platform has no configured builders. Drain the
                    // whole queue and log — these builds cannot be
                    // serviced.
                    while let Some(req) = queue.pop_front() {
                        error!(
                            "No system queue configured for platform {}; dropping build: {:?}",
                            platform, req
                        );
                    }
                    continue;
                };
                while !queue.is_empty() {
                    match sender.try_reserve() {
                        Ok(permit) => {
                            // Safe: is_empty() returned false, so
                            // pop_front() cannot be None.
                            permit.send(queue.pop_front().unwrap());
                        },
                        Err(_) => break, // downstream full; retry next tick
                    }
                }
            }

            // Step 3: if there is nothing to dispatch and nothing on
            // the receiver, block on `recv().await` to avoid spinning.
            // Otherwise sleep briefly so downstream consumers can
            // drain before we loop back to step 1.
            let all_empty = pending.values().all(|q| q.is_empty());
            if all_empty {
                match self.build_request_receiver.recv().await {
                    Some(build_request) => {
                        debug!("Received request for build: {:?}", &build_request);
                        classify_and_enqueue(
                            build_request,
                            default_platform.as_deref(),
                            &mut pending,
                        );
                    },
                    None => {
                        warn!("Build request channel closed");
                        return;
                    },
                }
            } else {
                dispatch_timer.tick().await;
            }
        }
    }
}

/// Classify a build request by target platform (handling `builtin`) and
/// push it onto the appropriate per-platform accumulator.  Requests
/// whose platform cannot be resolved (e.g. `builtin` when
/// `nix-instantiate` was unavailable at startup) are dropped with an
/// `error!`.
fn classify_and_enqueue(
    request: BuildRequest,
    default_platform: Option<&str>,
    pending: &mut HashMap<Platform, VecDeque<BuildRequest>>,
) {
    // 'system = "builtin";' is for <nix/fetchurl> and a few others
    // which are the first FOD to be made for a drv graph.
    let platform: Platform = if request.0.system == "builtin" {
        match default_platform {
            Some(p) => p.to_string(),
            None => {
                error!(
                    "Cannot route `builtin` build: default platform unknown (nix-instantiate \
                     unavailable); dropping: {:?}",
                    request
                );
                return;
            },
        }
    } else {
        request.0.system.clone()
    };

    pending.entry(platform).or_default().push_back(request);
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
