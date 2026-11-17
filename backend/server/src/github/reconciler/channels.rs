//! Reconciliation of release-channel promotion.
//!
//! Promotion is driven by events that can each be lost: the push to the
//! tracking branch (server down), the checkout/eval tasks it queues
//! (in-memory channels), the recorder's `JobsetComplete` (in-memory
//! channel), and the coalescer's pending SHA (kept only in memory). Each
//! pass reads the tracking branch's head from GitHub, makes sure the
//! head and the in-flight SHA are evaluated, and asks the ChannelService
//! to re-check the channel (`ChannelTask::Reconcile`).

use anyhow::{Context, Result};
use octocrab::Octocrab;
use tracing::warn;

use super::{Decision, EvalReconciler, GitRef, log_enqueue};
use crate::channels::types::ChannelTask;
use crate::config::{ChannelConfig, ChannelForge};
use crate::db::github::{HeadCommit, RepositoryInfo};
use crate::github::webhook::push::queue_channel_checkout;

/// Commits of a channel that must be evaluated for promotion to progress.
fn commits_to_evaluate<'a>(
    in_flight_sha: Option<&'a str>,
    tracking_head: &'a str,
    head_decided: bool,
) -> Vec<&'a str> {
    let mut shas: Vec<&str> = in_flight_sha.into_iter().collect();
    if !head_decided && in_flight_sha != Some(tracking_head) {
        shas.push(tracking_head);
    }
    shas
}

impl EvalReconciler {
    /// Reconcile every GitHub release channel tracking a branch of `repo`.
    pub(super) async fn reconcile_channels(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
    ) -> Result<()> {
        let Some(sender) = &self.channels.sender else {
            return Ok(());
        };
        let watching = self.channels.configs.values().filter(|c| {
            c.forge == ChannelForge::GitHub
                && c.owner.eq_ignore_ascii_case(&repo.owner)
                && c.repo == repo.repo_name
        });
        for channel in watching {
            let Some(head) = self.channel_needs_work(octocrab, repo, channel).await? else {
                continue;
            };
            let task = ChannelTask::Reconcile {
                channel: channel.clone(),
                tracking_head: head,
            };
            sender
                .send(task)
                .await
                .context("failed to send ChannelTask::Reconcile")?;
        }
        Ok(())
    }

    /// Queue evaluations the channel is missing. Returns the tracking
    /// head if the ChannelService should re-check the channel, `None` if
    /// there is nothing in flight and the head was already decided.
    async fn channel_needs_work(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
        channel: &ChannelConfig,
    ) -> Result<Option<String>> {
        let route = format!(
            "/repos/{}/{}/git/ref/heads/{}",
            repo.owner, repo.repo_name, channel.tracking_branch
        );
        let head_ref: GitRef = octocrab
            .get(route, None::<&()>)
            .await
            .with_context(|| format!("failed to read branch {}", channel.tracking_branch))?;
        let head = head_ref.object.sha;

        let pool = &self.db_service.pool;
        let channel_id = channel.channel_id();
        let in_flight = crate::db::channels::get_in_flight(&channel_id, pool).await?;
        let in_flight_sha = in_flight.as_ref().map(|r| r.tracking_sha.as_str());
        let head_decided = crate::db::channels::get_latest_for_sha(&channel_id, &head, pool)
            .await?
            .is_some_and(|r| r.is_terminal_decision());

        let shas = commits_to_evaluate(in_flight_sha, &head, head_decided);
        if shas.is_empty() {
            return Ok(None);
        }
        for sha in &shas {
            let is_in_flight = in_flight_sha == Some(*sha);
            self.ensure_channel_commit_evaluated(repo, channel, sha, is_in_flight)
                .await?;
        }
        Ok(Some(head))
    }

    /// Queue the evaluation of a tracking-branch commit if it was lost.
    async fn ensure_channel_commit_evaluated(
        &self,
        repo: &RepositoryInfo,
        channel: &ChannelConfig,
        sha: &str,
        is_in_flight: bool,
    ) -> Result<()> {
        let head = HeadCommit {
            owner: &repo.owner,
            repo_name: &repo.repo_name,
            sha,
        };
        let (decision, request) = self.head_decision(head).await?;
        match decision {
            Decision::Enqueue => {
                log_enqueue(head, &format!("channel {}", channel.name), request.as_ref());
                queue_channel_checkout(
                    &repo.owner,
                    &repo.repo_name,
                    &channel.tracking_branch,
                    sha,
                    true,
                    &self.git_sender,
                    &self.db_service,
                )
                .await
            },
            Decision::Exhausted if is_in_flight => {
                // No jobset will ever arrive, so the Evaluating row (and
                // every newer commit queued behind it) stays stuck.
                warn!(
                    "Channel {} is stuck: in-flight commit {} was never evaluated after \
                     repeated attempts; it needs manual intervention",
                    channel.name, sha
                );
                Ok(())
            },
            Decision::Done | Decision::InFlight | Decision::Exhausted => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_channel_with_decided_head_needs_nothing() {
        assert!(commits_to_evaluate(None, "head", true).is_empty());
    }

    #[test]
    fn undecided_head_is_evaluated() {
        assert_eq!(commits_to_evaluate(None, "head", false), vec!["head"]);
    }

    #[test]
    fn in_flight_head_is_evaluated_once() {
        assert_eq!(
            commits_to_evaluate(Some("head"), "head", false),
            vec!["head"]
        );
    }

    #[test]
    fn in_flight_and_newer_head_are_both_evaluated() {
        assert_eq!(
            commits_to_evaluate(Some("old"), "head", false),
            vec!["old", "head"]
        );
    }

    #[test]
    fn in_flight_is_rechecked_even_if_head_was_decided() {
        assert_eq!(commits_to_evaluate(Some("old"), "head", true), vec!["old"]);
    }
}
