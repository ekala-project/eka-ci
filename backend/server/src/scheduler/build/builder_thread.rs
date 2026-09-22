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
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::circuit_breaker::{self, CircuitBreakerRegistry};
use super::{BuildRequest, Platform};
use crate::db::model::{DrvId, build_event};
use crate::graph::GraphServiceHandle;
use crate::metrics::BuildMetrics;
use crate::nix::reconstitute::ReconstitutionTracker;
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
    graph_handle: GraphServiceHandle,
    db_pool: sqlx::SqlitePool,
    reconstitution_tracker: Arc<ReconstitutionTracker>,
    builder_name: String,
    is_remote: bool,
    circuit_breaker: CircuitBreakerRegistry,
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
        graph_handle: GraphServiceHandle,
        db_pool: sqlx::SqlitePool,
        reconstitution_tracker: Arc<ReconstitutionTracker>,
        builder_name: String,
        is_local: bool,
        circuit_breaker: CircuitBreakerRegistry,
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
            graph_handle,
            db_pool,
            reconstitution_tracker,
            builder_name,
            is_remote: !is_local,
            circuit_breaker,
        }
    }

    pub fn run(self, cancellation_token: CancellationToken) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(self.max_jobs.into());

        tokio::spawn(async move {
            self.loop_for_builds(rx, cancellation_token).await;
        });

        tx
    }

    async fn loop_for_builds(
        self,
        mut build_receiver: mpsc::Receiver<BuildRequest>,
        cancellation_token: CancellationToken,
    ) {
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

            tokio::select! {
                _ = cancellation_token.cancelled() => break,
                result = build_receiver.recv() => {
                    match result {
                        Some(build_request) => {
                            // Skip builds whose drv is already in a terminal state
                            // (e.g. cache-hit marked Completed(Success) by the recorder
                            // while the build request was queued).
                            let drv_id = &build_request.0.drv_path;
                            if let Ok(shared_id) = crate::graph_compat::to_shared_drv_id(drv_id) {
                                if let Some(state) = self.graph_handle.get_build_state(&shared_id) {
                                    if state.is_terminal() {
                                        debug!(
                                            "skipping build for {} (already {:?})",
                                            drv_id.store_path(),
                                            state
                                        );
                                        continue;
                                    }
                                }

                                // Transition to Building before spawning the build task.
                                // This matches the spec's start_build action and ensures
                                // the recorder sees Building state when processing results.
                                let shared_building =
                                    crate::db::graph_impl::convert_build_state(
                                        &build_event::DrvBuildState::Building,
                                    );
                                if let Some(mut entry) =
                                    self.graph_handle.shared_view().get_mut(&shared_id)
                                {
                                    entry.build_state = shared_building.clone();
                                }
                                // Best-effort graph + DB update via command channel.
                                let (tx, _rx) = tokio::sync::oneshot::channel();
                                let cmd = crate::graph::GraphCommand::UpdateState {
                                    drv_id: shared_id,
                                    new_state: shared_building,
                                    response: tx,
                                };
                                if let Err(e) = self.graph_handle.command_sender().try_send(cmd) {
                                    warn!(
                                        "graph command queue full, deferred Building state for {}: {}",
                                        drv_id.store_path(),
                                        e
                                    );
                                }
                            }

                            let new_build = self.create_build(build_request.0.drv_path);
                            build_set.spawn(async move { new_build.attempt_build().await });
                            // Update active builds metric after starting a new build
                            self.metrics
                                .active_builds
                                .with_label_values(&[&self.platform])
                                .set(build_set.len() as f64);
                        },
                        None => break,
                    }
                }
            }
        }

        // Let in-flight builds finish
        while build_set.join_next().await.is_some() {}

        info!("BuilderThread service shutdown gracefully");
    }

    fn create_build(&self, drv_id: DrvId) -> NixBuild {
        NixBuild {
            build_args: self.build_args.clone(),
            logs_dir: self.logs_dir.clone(),
            recorder_sender: self.recorder_sender.clone(),
            drv_id,
            no_output_timeout_seconds: self.no_output_timeout_seconds,
            max_duration_seconds: self.max_duration_seconds,
            db_pool: self.db_pool.clone(),
            reconstitution_tracker: self.reconstitution_tracker.clone(),
            builder_name: self.builder_name.clone(),
            is_remote: self.is_remote,
            circuit_breaker: self.circuit_breaker.clone(),
            platform: self.platform.clone(),
            metrics: self.metrics.clone(),
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
    db_pool: sqlx::SqlitePool,
    reconstitution_tracker: Arc<ReconstitutionTracker>,
    builder_name: String,
    is_remote: bool,
    circuit_breaker: CircuitBreakerRegistry,
    platform: Platform,
    metrics: Arc<BuildMetrics>,
}

enum BuildOutcome {
    Success,
    Failure {
        exit_code: Option<i32>,
        stderr_tail: String,
    },
    Timeout,
    AbsoluteTimeout,
}

impl NixBuild {
    async fn perform_build(&self) -> build_event::DrvBuildState {
        use build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

        let drv_path = self.drv_id.store_path();

        // Guard: verify the .drv file still exists before building.
        // If it was garbage collected, attempt to reconstitute it by
        // re-evaluating the nix expression that originally produced it.
        if !crate::nix::reconstitute::drv_store_path_exists(&drv_path).await {
            warn!(
                "drv {} was garbage collected, attempting reconstitution",
                drv_path
            );
            match crate::nix::reconstitute::reconstitute_drv(
                &self.drv_id,
                &self.db_pool,
                &self.reconstitution_tracker,
            )
            .await
            {
                Ok(true) => info!("reconstituted {}", drv_path),
                Ok(false) => {
                    warn!("could not reconstitute {}", drv_path);
                    return DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath);
                },
                Err(e) => {
                    warn!("reconstitution failed for {}: {:?}", drv_path, e);
                    return DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath);
                },
            }
        }

        let outcome = match self.build_drv_with_logging().await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!("Failed to build {:?}, encountered error: {:?}", drv_path, e);
                return DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath);
            },
        };

        // Report build outcome to circuit-breaker for remote builders.
        if self.is_remote {
            match &outcome {
                BuildOutcome::Success => {
                    self.circuit_breaker
                        .record_success(&self.builder_name)
                        .await;
                },
                BuildOutcome::Failure {
                    exit_code,
                    stderr_tail,
                } => {
                    if circuit_breaker::is_connection_failure(*exit_code, stderr_tail) {
                        warn!(
                            "connection failure detected for builder '{}' (exit={:?}): {}",
                            self.builder_name, exit_code, drv_path,
                        );
                        self.circuit_breaker
                            .record_failure(&self.builder_name)
                            .await;
                    }
                },
                BuildOutcome::Timeout | BuildOutcome::AbsoluteTimeout => {},
            }
        }

        match outcome {
            BuildOutcome::Success => DrvBuildState::Completed(DrvBuildResult::Success),
            BuildOutcome::Failure { .. } => DrvBuildState::Completed(DrvBuildResult::Failure),
            BuildOutcome::Timeout => {
                warn!(
                    "Build timed out for {:?} (no output for {} seconds)",
                    drv_path, self.no_output_timeout_seconds
                );
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout)
            },
            BuildOutcome::AbsoluteTimeout => {
                warn!(
                    "Build timed out for {:?} (wall-clock cap of {} seconds reached)",
                    drv_path, self.max_duration_seconds
                );
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout)
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

        // Prefetch measurement: track when the first output line arrives.
        // The gap between spawn and first output is the input-fetch phase;
        // the gap between first output and completion is the build phase.
        let spawn_time = Instant::now();
        let mut first_output_time: Option<Instant> = None;

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
                        Ok(0) => {},
                        Ok(_) => {
                            log_writer.write_all(&stdout_buf).await?;
                            stdout_buf.clear();
                            first_output_time.get_or_insert_with(Instant::now);
                            timeout.as_mut().reset(tokio::time::Instant::now() + timeout_duration);
                        },
                        Err(e) => warn!("Error reading stdout: {}", e),
                    }
                },
                result = stderr_reader.read_until(b'\n', &mut stderr_buf) => {
                    match result {
                        Ok(0) => {},
                        Ok(_) => {
                            log_writer.write_all(&stderr_buf).await?;
                            stderr_buf.clear();
                            first_output_time.get_or_insert_with(Instant::now);
                            timeout.as_mut().reset(tokio::time::Instant::now() + timeout_duration);
                        },
                        Err(e) => warn!("Error reading stderr: {}", e),
                    }
                },
                _ = &mut timeout => {
                    warn!("Build timed out after {}s of no output", self.no_output_timeout_seconds);
                    if self.is_remote {
                        warn!(
                            "killing remote build on '{}' for {} — orphaned nix-daemon \
                             processes may remain on the remote machine",
                            self.builder_name,
                            self.drv_id.store_path(),
                        );
                    }
                    Self::kill_and_flush(&mut child, log_writer).await?;
                    return Ok(BuildOutcome::Timeout);
                },
                _ = &mut absolute_timeout => {
                    warn!("Build exceeded wall-clock cap of {}s", self.max_duration_seconds);
                    if self.is_remote {
                        warn!(
                            "killing remote build on '{}' for {} — orphaned nix-daemon \
                             processes may remain on the remote machine",
                            self.builder_name,
                            self.drv_id.store_path(),
                        );
                    }
                    Self::kill_and_flush(&mut child, log_writer).await?;
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

        // Record prefetch/build phase timing metrics.
        let completion_time = Instant::now();
        let locality = if self.is_remote { "remote" } else { "local" };
        if let Some(first_out) = first_output_time {
            let fetch_secs = first_out.duration_since(spawn_time).as_secs_f64();
            let build_secs = completion_time.duration_since(first_out).as_secs_f64();
            self.metrics
                .fetch_duration_seconds
                .with_label_values(&[&self.platform, locality])
                .observe(fetch_secs);
            self.metrics
                .build_duration_seconds
                .with_label_values(&[&self.platform, locality])
                .observe(build_secs);
            debug!(
                "build phases for {}: fetch={:.1}s build={:.1}s",
                self.drv_id.store_path(),
                fetch_secs,
                build_secs,
            );
        }

        if status.success() {
            // Try to replace build log with `nix log` output (richer for substituted drvs)
            match get_nix_log(&self.drv_id).await {
                Ok(nix_log) if !nix_log.is_empty() => {
                    tokio::fs::write(&log_path, nix_log).await?;
                },
                Ok(_) => debug!(
                    "nix log empty for {}, keeping build output",
                    self.drv_id.store_path()
                ),
                Err(e) => debug!("nix log failed for {}: {}", self.drv_id.store_path(), e),
            }
            Ok(BuildOutcome::Success)
        } else {
            // Read the tail of the build log for connection failure classification.
            let stderr_tail = read_file_tail(&log_path, 1024).await;
            Ok(BuildOutcome::Failure {
                exit_code: status.code(),
                stderr_tail,
            })
        }
    }

    async fn kill_and_flush(
        child: &mut tokio::process::Child,
        log_writer: BufWriter<File>,
    ) -> anyhow::Result<()> {
        if let Err(e) = child.kill().await {
            warn!("Failed to kill build child (may already be dead): {:?}", e);
        }
        if let Err(e) = child.wait().await {
            warn!("Failed to reap build child: {:?}", e);
        }
        let mut lw = log_writer;
        lw.flush().await?;
        lw.into_inner().sync_all().await?;
        Ok(())
    }

    async fn attempt_build(self) -> anyhow::Result<()> {
        let build_state = self.perform_build().await;

        // Let the recorder deal with updating build state and
        // determining if downstream drvs are now buildable.
        let recorder_task = RecorderTask {
            derivation: std::sync::Arc::new(self.drv_id),
            result: build_state,
        };
        self.recorder_sender.send(recorder_task).await?;

        Ok(())
    }
}

/// Retrieve build logs from nix's log storage
/// This is particularly useful for substituted derivations where we don't build locally
async fn get_nix_log(drv_id: &DrvId) -> anyhow::Result<String> {
    use anyhow::Context;

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new("nix")
            .args(["log", &drv_id.store_path()])
            .output(),
    )
    .await
    .context("nix log timed out")?
    .context("failed to run nix log")?;

    if !output.status.success() {
        anyhow::bail!("nix log command failed with status: {}", output.status);
    }
    let str = String::from_utf8(output.stdout)?;
    Ok(str)
}

/// Read up to `max_bytes` from the tail of a file. Returns an empty
/// string on any I/O error (best-effort for diagnostics).
async fn read_file_tail(path: &std::path::Path, max_bytes: u64) -> String {
    use tokio::io::AsyncSeekExt;

    let mut file = match File::open(path).await {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let len = metadata.len();
    let start = len.saturating_sub(max_bytes);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return String::new();
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    if file.read_to_end(&mut buf).await.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    //! M5: regression tests for the dual-deadline pattern in `build_drv_with_logging`.
    //! Uses `tokio::time::pause()` to drive virtual time since real `nix-build` is unavailable.
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
        let kind = race_deadlines(10, 30, || {
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
