use serde::{Deserialize, Serialize};

use crate::ci::config::Skill;

/// Task to evaluate a skill for a specific commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTask {
    pub skill_name: String,
    pub owner: String,
    pub repo_name: String,
    pub sha: String,
    /// Base commit SHA for generating the diff.
    pub base_sha: String,
    pub config: Skill,
    #[serde(default)]
    pub pr_title: Option<String>,
    #[serde(default)]
    pub pr_description: Option<String>,
    #[serde(default)]
    pub pr_author: Option<String>,
}

/// Result of a skill evaluation, sent to the GitHub service for
/// check-run updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillResultMessage {
    pub skill_name: String,
    pub owner: String,
    pub repo_name: String,
    pub sha: String,
    /// Whether the skill passed (decision == pass).
    pub success: bool,
    /// Whether the gate is advisory (neutral conclusion).
    pub advisory: bool,
    /// True when the model call or JSON parse failed.
    pub error: bool,
    /// Reason string from the verdict or an error message.
    pub reason: String,
    /// Serialized JSON array of findings for the check-run body.
    pub findings_json: String,
    /// Model ID that was used for the evaluation.
    pub model_id: String,
    /// Duration of the model call in milliseconds.
    pub duration_ms: i64,
    /// GitHub check-run ID (populated from DB after initial creation).
    pub check_run_id: i64,
}
