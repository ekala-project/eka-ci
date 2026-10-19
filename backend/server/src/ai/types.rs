use serde::{Deserialize, Serialize};

/// Structured verdict returned by a skill evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVerdict {
    pub decision: SkillDecision,
    pub reason: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// Pass or fail decision from a skill evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillDecision {
    Pass,
    Fail,
}

/// A single finding from a skill evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Severity level of a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "critical"),
            Severity::Warning => write!(f, "warning"),
            Severity::Info => write!(f, "info"),
        }
    }
}

impl std::fmt::Display for SkillDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillDecision::Pass => write!(f, "pass"),
            SkillDecision::Fail => write!(f, "fail"),
        }
    }
}

/// The JSON schema string for the verdict, used in prompts when the
/// provider does not support native structured output.
pub const VERDICT_SCHEMA_PROMPT: &str = r#"You MUST respond with a JSON object matching this exact schema (no markdown fencing, no extra text):
{
  "decision": "pass" or "fail",
  "reason": "string explaining the decision",
  "findings": [
    {
      "severity": "critical" | "warning" | "info",
      "description": "string describing the finding",
      "file": "optional file path",
      "line": optional line number
    }
  ]
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_pass_verdict() {
        let json = r#"{
            "decision": "pass",
            "reason": "All checks passed",
            "findings": []
        }"#;
        let verdict: SkillVerdict = serde_json::from_str(json).unwrap();
        assert_eq!(verdict.decision, SkillDecision::Pass);
        assert!(verdict.findings.is_empty());
    }

    #[test]
    fn deserialize_fail_verdict_with_findings() {
        let json = r#"{
            "decision": "fail",
            "reason": "Security issues found",
            "findings": [
                {
                    "severity": "critical",
                    "description": "SQL injection in query builder",
                    "file": "src/db.rs",
                    "line": 42
                },
                {
                    "severity": "info",
                    "description": "Consider using parameterized queries"
                }
            ]
        }"#;
        let verdict: SkillVerdict = serde_json::from_str(json).unwrap();
        assert_eq!(verdict.decision, SkillDecision::Fail);
        assert_eq!(verdict.findings.len(), 2);
        assert_eq!(verdict.findings[0].severity, Severity::Critical);
        assert_eq!(verdict.findings[0].file.as_deref(), Some("src/db.rs"));
        assert_eq!(verdict.findings[0].line, Some(42));
        assert!(verdict.findings[1].file.is_none());
        assert!(verdict.findings[1].line.is_none());
    }

    #[test]
    fn deserialize_verdict_without_findings_field() {
        let json = r#"{"decision": "pass", "reason": "ok"}"#;
        let verdict: SkillVerdict = serde_json::from_str(json).unwrap();
        assert!(verdict.findings.is_empty());
    }
}
