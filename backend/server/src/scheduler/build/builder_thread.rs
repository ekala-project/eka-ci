use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{debug, error, warn};

use super::{BuildRequest, Platform};
use crate::db::model::{DrvId, build_event};
use crate::metrics::BuildMetrics;
use crate::scheduler::recorder::RecorderTask;

pub struct BuilderThread {
    build_args: [String; 2],
    max_jobs: u8,
    logs_dir: PathBuf,
    recorder_sender: mpsc::Sender<RecorderTask>,
    platform: Platform,
    metrics: Arc<BuildMetrics>,
    no_output_timeout_seconds: u64,
}

impl BuilderThread {
    pub fn init(
        build_args: [String; 2],
        max_jobs: u8,
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        platform: Platform,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
    ) -> Self {
        Self {
            build_args,
            max_jobs,
            logs_dir,
            recorder_sender,
            platform,
            metrics,
            no_output_timeout_seconds,
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
                    _ => {},
                }
                // Update active builds metric after completing a build
                self.metrics
                    .active_builds
                    .with_label_values(&[&self.platform])
                    .set(build_set.len() as f64);
            }

            if let Some(build_request) = build_receiver.recv().await {
                let new_build = self.create_build(build_request.0.drv_path);
                build_set.spawn(async move { new_build.attempt_build().await });
                // Update active builds metric after starting a new build
                self.metrics
                    .active_builds
                    .with_label_values(&[&self.platform])
                    .set(build_set.len() as f64);
            } else {
                interval.tick().await;
            }
        }
    }

    fn create_build(&self, drv_id: DrvId) -> NixBuild {
        NixBuild {
            build_args: self.build_args.clone(),
            logs_dir: self.logs_dir.clone(),
            recorder_sender: self.recorder_sender.clone(),
            drv_id,
            no_output_timeout_seconds: self.no_output_timeout_seconds,
        }
    }
}

struct NixBuild {
    build_args: [String; 2],
    logs_dir: PathBuf,
    recorder_sender: mpsc::Sender<RecorderTask>,
    drv_id: DrvId,
    no_output_timeout_seconds: u64,
}

enum BuildOutcome {
    Success,
    Failure,
    Timeout,
}

impl NixBuild {
    async fn perform_build(&self) -> build_event::DrvBuildState {
        use build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

        let drv_path = self.drv_id.store_path();
        match self.build_drv_with_logging().await {
            Ok(BuildOutcome::Success) => {
                debug!("Successfully built {:?}", drv_path);
                DrvBuildState::Completed(DrvBuildResult::Success)
            },
            Ok(BuildOutcome::Failure) => {
                debug!("Build failed for {:?}", drv_path);
                DrvBuildState::Completed(DrvBuildResult::Failure)
            },
            Ok(BuildOutcome::Timeout) => {
                warn!(
                    "Build timed out for {:?} (no output for {} seconds)",
                    drv_path, self.no_output_timeout_seconds
                );
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout)
            },

            // Err doesn't denote process failure, rather process construction or logging error
            Err(e) => {
                warn!("Failed to build {:?}, encountered error: {:?}", drv_path, e);
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath)
            },
        }
    }

    /// Build the derivation and stream logs to disk
    /// Returns BuildOutcome indicating success, failure, or timeout
    async fn build_drv_with_logging(&self) -> anyhow::Result<BuildOutcome> {
        use tokio::io::AsyncBufReadExt;

        debug!("Building {} drv", self.drv_id.store_path());

        // Create log directory: {logs_dir}/{drv_hash}/
        let drv_hash = self.drv_id.drv_hash();
        let log_subdir = self.logs_dir.join(drv_hash);
        tokio::fs::create_dir_all(&log_subdir).await?;

        // Create log file: {logs_dir}/{drv_hash}/build.log
        let log_filename = "build.log";
        let log_path = log_subdir.join(log_filename);
        let log_file = File::create(&log_path).await?;
        let mut log_writer = BufWriter::new(log_file);

        // Spawn nix-build with stdout/stderr redirected
        let mut child = Command::new("nix-build")
            .args([self.drv_id.store_path()])
            .args(&self.build_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Stream stdout and stderr to log file
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut stdout_reader = tokio::io::BufReader::new(stdout);
        let mut stderr_reader = tokio::io::BufReader::new(stderr);

        // Interleave stdout and stderr into log file
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        // Create a timeout that resets whenever we receive output
        let timeout_duration = Duration::from_secs(self.no_output_timeout_seconds);
        let mut timeout = Box::pin(sleep(timeout_duration));

        loop {
            tokio::select! {
                result = stdout_reader.read_until(b'\n', &mut stdout_buf) => {
                    match result {
                        Ok(0) => {}, // EOF
                        Ok(_) => {
                            log_writer.write_all(&stdout_buf).await?;
                            stdout_buf.clear();
                            // Reset timeout on output
                            timeout.as_mut().reset(tokio::time::Instant::now() + timeout_duration);
                        },
                        Err(e) => warn!("Error reading stdout: {}", e),
                    }
                },
                result = stderr_reader.read_until(b'\n', &mut stderr_buf) => {
                    match result {
                        Ok(0) => {}, // EOF
                        Ok(_) => {
                            log_writer.write_all(&stderr_buf).await?;
                            stderr_buf.clear();
                            // Reset timeout on output
                            timeout.as_mut().reset(tokio::time::Instant::now() + timeout_duration);
                        },
                        Err(e) => warn!("Error reading stderr: {}", e),
                    }
                },
                _ = &mut timeout => {
                    // Timeout occurred - kill the process
                    warn!("Build timed out after {} seconds of no output, killing process", self.no_output_timeout_seconds);
                    let _ = child.kill().await;

                    // Flush and close log file
                    log_writer.flush().await?;
                    let log_file = log_writer.into_inner();
                    log_file.sync_all().await?;

                    return Ok(BuildOutcome::Timeout);
                },
            }

            // Check if process has exited
            if let Ok(Some(_)) = child.try_wait() {
                // Drain remaining output
                stdout_reader.read_to_end(&mut stdout_buf).await?;
                log_writer.write_all(&stdout_buf).await?;

                stderr_reader.read_to_end(&mut stderr_buf).await?;
                log_writer.write_all(&stderr_buf).await?;

                break;
            }
        }

        // Wait for child process to complete
        let status = child.wait().await?;

        // Flush and sync log file
        log_writer.flush().await?;
        let log_file = log_writer.into_inner();
        log_file.sync_all().await?;

        debug!(
            "Build log for {} written to {}",
            self.drv_id.store_path(),
            log_path.display()
        );

        if status.success() {
            Ok(BuildOutcome::Success)
        } else {
            Ok(BuildOutcome::Failure)
        }
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
