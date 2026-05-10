// Release-channel support module.
//
// PR 2: pure helper `match_push_channels` used by the push-webhook
//        handlers to look up channels watching a particular
//        `(forge, owner, repo, branch)` tuple.
//
// PR 3: ChannelService skeleton + the pure coalescer + the pure
//        promotion evaluator. The evaluator and coalescer are kept in
//        sibling modules so they remain unit-testable without the
//        async runtime / sqlite pool.

use std::collections::HashMap;

use crate::config::{ChannelConfig, ChannelForge};

pub mod coalescer;
pub mod evaluator;
pub mod service;
pub mod types;

pub use service::ChannelService;

/// Return the channels whose `tracking_key()` matches the supplied
/// `(forge, owner, repo, branch)`. The match is intentionally
/// case-sensitive on `repo` and `branch` (git is case-sensitive) but
/// not on `owner` (GitHub treats org/user logins case-insensitively).
///
/// Returns an empty `Vec` when no channels are configured or no
/// channel watches the supplied combination — the caller short-circuits
/// in that case rather than emitting work to downstream services.
pub fn match_push_channels<'a>(
    channels: &'a HashMap<String, ChannelConfig>,
    forge: &ChannelForge,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Vec<&'a ChannelConfig> {
    channels
        .values()
        .filter(|c| {
            &c.forge == forge
                && c.owner.eq_ignore_ascii_case(owner)
                && c.repo == repo
                && c.tracking_branch == branch
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(forge: ChannelForge, owner: &str, repo: &str, name: &str, tracking: &str) -> ChannelConfig {
        ChannelConfig {
            forge,
            owner: owner.to_string(),
            repo: repo.to_string(),
            name: name.to_string(),
            tracking_branch: tracking.to_string(),
            target_branch: format!("{name}-unstable"),
            required: Vec::new(),
            packages: Vec::new(),
            dry_run: false,
        }
    }

    fn registry(items: Vec<ChannelConfig>) -> HashMap<String, ChannelConfig> {
        items
            .into_iter()
            .map(|c| (c.channel_id(), c))
            .collect()
    }

    #[test]
    fn matches_single_channel_on_exact_tuple() {
        let map = registry(vec![ch(
            ChannelForge::GitHub,
            "ekacorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "ekacorp", "ekapkgs", "master");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "stable");
    }

    #[test]
    fn owner_match_is_case_insensitive() {
        // GitHub login matching is case-insensitive in practice; webhooks
        // sometimes deliver a canonicalised casing that differs from
        // what an operator typed into config. Allow both to match.
        let map = registry(vec![ch(
            ChannelForge::GitHub,
            "EkaCorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "ekacorp", "ekapkgs", "master");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn branch_match_is_case_sensitive() {
        // Git refs are case-sensitive: "Master" must NOT match "master".
        let map = registry(vec![ch(
            ChannelForge::GitHub,
            "ekacorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "ekacorp", "ekapkgs", "Master");
        assert!(hits.is_empty());
    }

    #[test]
    fn does_not_match_different_branch() {
        let map = registry(vec![ch(
            ChannelForge::GitHub,
            "ekacorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits = match_push_channels(
            &map,
            &ChannelForge::GitHub,
            "ekacorp",
            "ekapkgs",
            "feature-x",
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn does_not_match_different_forge() {
        // A push from github.com must not trigger a Gitea channel even
        // if owner/repo/branch line up.
        let map = registry(vec![ch(
            ChannelForge::Gitea {
                domain: "gitea.example.com".to_string(),
            },
            "ekacorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "ekacorp", "ekapkgs", "master");
        assert!(hits.is_empty());
    }

    #[test]
    fn does_not_match_different_gitea_domain() {
        let map = registry(vec![ch(
            ChannelForge::Gitea {
                domain: "gitea.a.example".to_string(),
            },
            "ekacorp",
            "ekapkgs",
            "stable",
            "master",
        )]);
        let hits = match_push_channels(
            &map,
            &ChannelForge::Gitea {
                domain: "gitea.b.example".to_string(),
            },
            "ekacorp",
            "ekapkgs",
            "master",
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn matches_multiple_channels_on_same_tracking_branch() {
        // The same tracking-branch can feed multiple channels (e.g.
        // master -> ekapkgs-unstable AND master -> ekapkgs-staging).
        let map = registry(vec![
            ch(
                ChannelForge::GitHub,
                "ekacorp",
                "ekapkgs",
                "stable",
                "master",
            ),
            ch(
                ChannelForge::GitHub,
                "ekacorp",
                "ekapkgs",
                "staging",
                "master",
            ),
        ]);
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "ekacorp", "ekapkgs", "master");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn empty_registry_returns_empty() {
        let map: HashMap<String, ChannelConfig> = HashMap::new();
        let hits =
            match_push_channels(&map, &ChannelForge::GitHub, "anyone", "anything", "any");
        assert!(hits.is_empty());
    }
}
