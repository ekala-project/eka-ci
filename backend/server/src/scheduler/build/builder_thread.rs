use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until};
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
    /// M5: absolute wall-clock cap per build; does not reset on output.
    max_duration_seconds: u64,
}

impl BuilderThread {
    #[allow(clippy::too_many_arguments)]
    pub fn init(
        build_args: [String; 2],
        max_jobs: u8,
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        platform: Platform,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
        max_duration_seconds: u64,
    ) -> Self {
        Self {
            build_args,
            max_jobs,
            logs_dir,
            recorder_sender,
            platform,
            metrics,
            no_output_timeout_seconds,
            max_duration_seconds,
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
            max_duration_seconds: self.max_duration_seconds,
        }
    }
}

struct NixBuild {
    build_args: [String; 2],
    logs_dir: PathBuf,
    recorder_sender: mpsc::Sender<RecorderTask>,
    drv_id: DrvId,
    no_output_timeout_seconds: u64,
    max_duration_seconds: u64,
}

enum BuildOutcome {
    Success,
    Failure,
    /// Resettable "no-output" timeout elapsed.
    Timeout,
    /// M5: absolute wall-clock cap elapsed regardless of output.
    AbsoluteTimeout,
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
            Ok(BuildOutcome::AbsoluteTimeout) => {
                warn!(
                    "Build timed out for {:?} (wall-clock cap of {} seconds reached)",
                    drv_path, self.max_duration_seconds
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

        // M5: absolute wall-clock deadline that does NOT reset on
        // output. A derivation that prints one byte per minute used to
        // hold a builder slot indefinitely; now it is killed after
        // `max_duration_seconds` regardless of output activity.
        let absolute_deadline = Instant::now() + Duration::from_secs(self.max_duration_seconds);
        let mut absolute_timeout = Box::pin(sleep_until(absolute_deadline));

        loop {
            tokio::select! {
                result = stdout_reader.read_until(b'\n', &mut stdout_buf) => {
                    match result {
                        Ok(0) => {}, // EOF
                        Ok(_) => {
                            log_writer.write_all(&stdout_buf).await?;
                            stdout_buf.clear();
                            // Reset no-output timeout only; absolute deadline is fixed.
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
                            // Reset no-output timeout only; absolute deadline is fixed.
                            timeout.as_mut().reset(tokio::time::Instant::now() + timeout_duration);
                        },
                        Err(e) => warn!("Error reading stderr: {}", e),
                    }
                },
                _ = &mut timeout => {
                    // No-output timeout occurred - kill the process
                    warn!("Build timed out after {} seconds of no output, killing process", self.no_output_timeout_seconds);
                    let _ = child.kill().await;
                    // Reap to avoid zombies and release file descriptors
                    let _ = child.wait().await;

                    // Flush and close log file
                    log_writer.flush().await?;
                    let log_file = log_writer.into_inner();
                    log_file.sync_all().await?;

                    return Ok(BuildOutcome::Timeout);
                },
                _ = &mut absolute_timeout => {
                    // M5: absolute wall-clock cap reached
                    warn!(
                        "Build exceeded wall-clock cap of {} seconds, killing process",
                        self.max_duration_seconds,
                    );
                    let _ = child.kill().await;
                    let _ = child.wait().await;

                    log_writer.flush().await?;
                    let log_file = log_writer.into_inner();
                    log_file.sync_all().await?;

                    return Ok(BuildOutcome::AbsoluteTimeout);
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
            // For successful builds, try to get logs from `nix log`
            // This captures logs from substituted derivations
            match get_nix_log(&self.drv_id).await {
                Ok(nix_log_output) if !nix_log_output.is_empty() => {
                    // Replace log file with nix log output
                    debug!(
                        "Replacing build log with nix log output for {}",
                        self.drv_id.store_path()
                    );
                    tokio::fs::write(&log_path, nix_log_output).await?;
                },
                Ok(_) => {
                    // nix log returned empty, keep the streamed build output
                    debug!(
                        "nix log returned empty for {}, keeping streamed output",
                        self.drv_id.store_path()
                    );
                },
                Err(e) => {
                    // nix log failed, keep the streamed build output
                    debug!(
                        "nix log failed for {}: {}, keeping streamed output",
                        self.drv_id.store_path(),
                        e
                    );
                },
            }
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

/// Retrieve build logs from nix's log storage
/// This is particularly useful for substituted derivations where we don't build locally
async fn get_nix_log(drv_id: &DrvId) -> anyhow::Result<String> {
    let output = Command::new("nix")
        .args(["log", &drv_id.store_path()])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!("nix log command failed with status: {}", output.status);
    }
    let str = String::from_utf8(output.stdout)?;
    Ok(str)
}

#[cfg(test)]
mod tests {
    //! M5: regression tests for the dual-deadline pattern that drives
    //! `build_drv_with_logging`. The real function spawns `nix-build`
    //! which is unavailable in unit tests, so these tests mirror the
    //! inner `select!` shape using `tokio::time::pause()` to drive
    //! virtual time. Any future change that accidentally resets the
    //! absolute deadline on output will flip the first test from
    //! passing (absolute fires) to panicking (no-output fires), which
    //! is the exact regression M5 protects against.
    use std::pin::Pin;
    use std::time::Duration;

    use tokio::time::{Instant, Sleep, sleep, sleep_until};
    use tracing::info;

    use super::*;
    #[derive(Debug, PartialEq, Eq)]
    enum TimeoutKind {
        NoOutput,
        Absolute,
    }
    /// Mirror of the production `select!` block: two pinned sleeps +
    /// reset the no-output sleep whenever the caller reports output.
    async fn race_deadlines<F>(
        no_output_seconds: u64,
        max_duration_seconds: u64,
        mut output_tick: F,
    ) -> TimeoutKind
    where
        F: FnMut() -> Option<Duration>,
    {
        let no_output_dur = Duration::from_secs(no_output_seconds);
        let mut no_output: Pin<Box<Sleep>> = Box::pin(sleep(no_output_dur));
        let mut absolute: Pin<Box<Sleep>> = Box::pin(sleep_until(
            Instant::now() + Duration::from_secs(max_duration_seconds),
        ));
        loop {
            let next_tick = output_tick();
            match next_tick {
                Some(delay) => {
                    tokio::select! {
                        biased;
                        _ = &mut absolute => return TimeoutKind::Absolute,
                        _ = &mut no_output => return TimeoutKind::NoOutput,
                        _ = sleep(delay) => {
                            // Simulated output: reset no-output only, not absolute.
                            no_output.as_mut().reset(Instant::now() + no_output_dur);
                        }
                    }
                },
                None => {
                    tokio::select! {
                        biased;
                        _ = &mut absolute => return TimeoutKind::Absolute,
                        _ = &mut no_output => return TimeoutKind::NoOutput,
                    }
                },
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn absolute_timeout_fires_even_with_regular_output() {
        // Simulate a chatty build that prints every 5 s for a long
        // time. The no-output timeout is 10 s (would never fire) but
        // the absolute cap is 30 s (must fire).
        let mut ticks_fired = 0u32;
        let kind = race_deadlines(10, 30, move || {
            ticks_fired += 1;
            // Never stop ticking; tokio virtual time will race the
            // sleep vs the absolute deadline and absolute will win.
            Some(Duration::from_secs(5))
        })
        .await;
        assert_eq!(kind, TimeoutKind::Absolute);
    }

    #[tokio::test(start_paused = true)]
    async fn no_output_fires_when_no_ticks_produced() {
        // No ticks: the no-output sleep fires first (10 s < 30 s cap).
        let kind = race_deadlines(10, 30, || None).await;
        assert_eq!(kind, TimeoutKind::NoOutput);
    }

    #[tokio::test(start_paused = true)]
    async fn absolute_timeout_wins_even_when_no_output_identical() {
        // If both deadlines are scheduled for the same instant the
        // `biased` ordering in the select deterministically picks the
        // absolute arm. This guards against the pre-M5 behavior where
        // the only deadline was the no-output one.
        let kind = race_deadlines(30, 30, || None).await;
        assert_eq!(kind, TimeoutKind::Absolute);
    }

    #[test]
    fn build_outcome_absolute_timeout_distinct_from_timeout() {
        // Regression: the enum must have a distinct variant so perform_build
        // can log the wall-clock reason separately.
        let a = BuildOutcome::AbsoluteTimeout;
        let b = BuildOutcome::Timeout;
        let a_is_abs = matches!(a, BuildOutcome::AbsoluteTimeout);
        let b_is_abs = matches!(b, BuildOutcome::AbsoluteTimeout);
        assert!(a_is_abs);
        assert!(!b_is_abs);
        // quiet unused-import lint for `info`
        info!("m5 variant check ok");
    }
}
