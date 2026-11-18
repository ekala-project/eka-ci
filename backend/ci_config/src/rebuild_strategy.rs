use serde::{Deserialize, Serialize};

/// Which of a PR's rebuilt packages a job builds.
///
/// Set per job with `"rebuild_strategy"` in `.ekaci/config.json`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RebuildStrategy {
    /// Build every new or changed job, and everything they depend on.
    #[default]
    All,
    /// Build only the packages a PR changes and the jobs that consume them
    /// directly.
    ///
    /// Seeds are rebuilt jobs that are dependency-graph roots (no rebuilt
    /// job among their direct `inputDrvs`), whose `meta.position` file is in
    /// the PR's `git diff`, whose version changed, or that are new. Each
    /// seed's direct referrers among the rebuilt jobs are built as well;
    /// every other rebuilt job is skipped.
    #[serde(alias = "direct_referrers")]
    DirectReferrers,
}

impl RebuildStrategy {
    /// The strategy configured in a serialized job config, defaulting to
    /// [`RebuildStrategy::All`] when it is absent or unparseable.
    pub fn from_job_config_json(config_json: Option<&str>) -> Self {
        #[derive(Deserialize)]
        struct Partial {
            #[serde(default)]
            rebuild_strategy: RebuildStrategy,
        }
        config_json
            .and_then(|json| serde_json::from_str::<Partial>(json).ok())
            .map(|partial| partial.rebuild_strategy)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::RebuildStrategy;
    use crate::{CIConfig, Job};

    fn parse_job(json: &str) -> Job {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn defaults_to_all() {
        let job = parse_job(r#"{ "file": "a.nix" }"#);
        assert_eq!(job.rebuild_strategy, RebuildStrategy::All);
    }

    #[test]
    fn parses_direct_referrers() {
        let job = parse_job(r#"{ "file": "a.nix", "rebuild_strategy": "direct-referrers" }"#);
        assert_eq!(job.rebuild_strategy, RebuildStrategy::DirectReferrers);
        let job = parse_job(r#"{ "file": "a.nix", "rebuild_strategy": "direct_referrers" }"#);
        assert_eq!(job.rebuild_strategy, RebuildStrategy::DirectReferrers);
    }

    #[test]
    fn rejects_unknown_strategy() {
        let result = serde_json::from_str::<Job>(r#"{ "file": "a.nix", "rebuild_strategy": "x" }"#);
        assert!(result.is_err());
    }

    #[test]
    fn round_trips_through_job_config_json() {
        let job = parse_job(r#"{ "file": "a.nix", "rebuild_strategy": "direct-referrers" }"#);
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains(r#""rebuild_strategy":"direct-referrers""#));
        assert_eq!(
            RebuildStrategy::from_job_config_json(Some(&json)),
            RebuildStrategy::DirectReferrers
        );
    }

    #[test]
    fn job_config_json_falls_back_to_all() {
        assert_eq!(
            RebuildStrategy::from_job_config_json(None),
            RebuildStrategy::All
        );
        assert_eq!(
            RebuildStrategy::from_job_config_json(Some("not json")),
            RebuildStrategy::All
        );
        assert_eq!(
            RebuildStrategy::from_job_config_json(Some(r#"{"file":"a.nix"}"#)),
            RebuildStrategy::All
        );
    }

    #[test]
    fn rebuild_strategy_is_not_an_unknown_key() {
        let json = r#"{ "jobs": { "pkgs": { "file": "a.nix", "rebuild_strategy": "direct-referrers" } } }"#;
        let config = CIConfig::from_str(json).unwrap();
        assert!(
            config.unknown_keys().is_empty(),
            "{:?}",
            config.unknown_keys()
        );
    }
}
