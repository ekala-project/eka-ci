///! Platform-agnostic jobset data abstraction.
///!
///! This module provides a shared representation of jobset metadata that
///! platform services (GitHub, GitLab, Gitea) populate from their respective
///! database tables before passing to shared logic like change_summary.

/// Platform-agnostic jobset metadata.
///
/// Each platform service queries its own jobset table (GitHubJobSets,
/// GitLabJobSets, etc.) and constructs this struct to pass to shared code.
/// This allows change_summary and other shared logic to remain
/// platform-neutral while the database layer stays fully duplicated.
#[derive(Debug, Clone)]
pub struct JobsetData {
    /// Repository owner (GitHub org/user, GitLab group, etc.)
    pub owner: String,

    /// Repository name
    pub repo: String,

    /// Platform domain (e.g., "github.com", "gitlab.example.com")
    pub domain: String,

    /// Git commit SHA
    pub sha: String,

    /// Job name (e.g., "ci", "nixpkgs-eval")
    pub job: String,

    /// Serialized CI config JSON for post-build hooks.
    /// Corresponds to the `config_json` column in platform jobset tables.
    pub config_json: Option<String>,
}

impl JobsetData {
    /// Construct a new JobsetData from components.
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        domain: impl Into<String>,
        sha: impl Into<String>,
        job: impl Into<String>,
        config_json: Option<String>,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            domain: domain.into(),
            sha: sha.into(),
            job: job.into(),
            config_json,
        }
    }

    /// Returns the full repository identifier in the format "domain/owner/repo".
    pub fn full_repo_id(&self) -> String {
        format!("{}/{}/{}", self.domain, self.owner, self.repo)
    }

    /// Returns the repository identifier in the format "owner/repo".
    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobset_data_construction() {
        let data = JobsetData::new(
            "nixos",
            "nixpkgs",
            "github.com",
            "abc123",
            "ci",
            Some(r#"{"hooks":[]}"#.to_string()),
        );

        assert_eq!(data.owner, "nixos");
        assert_eq!(data.repo, "nixpkgs");
        assert_eq!(data.domain, "github.com");
        assert_eq!(data.sha, "abc123");
        assert_eq!(data.job, "ci");
        assert!(data.config_json.is_some());
    }

    #[test]
    fn full_repo_id_formatting() {
        let data = JobsetData::new(
            "owner",
            "repo",
            "gitlab.example.com",
            "sha",
            "job",
            None,
        );

        assert_eq!(data.full_repo_id(), "gitlab.example.com/owner/repo");
        assert_eq!(data.repo_id(), "owner/repo");
    }
}
