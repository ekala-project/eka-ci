//! Details shown on the check run of a successful build.
//!
//! After a build succeeds, the recorder's size checker measures its outputs
//! and sends a [`BuildReport`] in `GitHubTask::ReportBuildSuccess`. The
//! report replaces the plain success output with the output paths, their
//! output and closure sizes, how long the build took and how the sizes
//! compare with the base branch. When a size grew beyond its job's
//! threshold the check run is set to neutral and the size warning leads the
//! summary.

mod handler;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use super::size_warning::{self, SizeWarningReport};
use crate::db::size::{SizeComparison, SizeMetric};
use crate::nix::size::format_size;

/// What is known about a successful build of one derivation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildReport {
    /// `pname version`, when known.
    #[serde(default)]
    pub package: Option<String>,
    /// System the derivation was built for.
    #[serde(default)]
    pub system: String,
    /// Built by eka-ci, as opposed to substituted from a binary cache.
    #[serde(default)]
    pub built_locally: bool,
    /// Seconds from the last start of the build to its success.
    #[serde(default)]
    pub duration_secs: Option<i64>,
    /// The derivation's outputs, `out` first and the rest by name.
    #[serde(default)]
    pub outputs: Vec<OutputReport>,
    /// Sum of the outputs' NAR sizes, if they could be measured.
    #[serde(default)]
    pub total_output_size: Option<u64>,
    /// Size of the outputs' combined runtime closure (shared paths counted
    /// once), if it could be measured.
    #[serde(default)]
    pub total_closure_size: Option<u64>,
    /// Comparisons against base-branch baselines, one per job with a
    /// `size_check`. Exceeded ones make the check run neutral.
    #[serde(default)]
    pub comparisons: Vec<SizeComparison>,
}

/// One output of a successful build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputReport {
    /// Output name, e.g. `out` or `dev`.
    pub name: String,
    /// Full store path.
    pub path: String,
    /// NAR size of the output path itself.
    #[serde(default)]
    pub nar_size: Option<u64>,
    /// NAR size of the output and its runtime dependencies.
    #[serde(default)]
    pub closure_size: Option<u64>,
    /// Number of store paths in the output's runtime closure.
    #[serde(default)]
    pub closure_paths: Option<usize>,
}

impl BuildReport {
    /// The comparisons whose size grew beyond the threshold.
    pub fn warnings(&self) -> Vec<&SizeComparison> {
        self.comparisons.iter().filter(|c| c.exceeded()).collect()
    }

    /// Sort outputs the way they are shown: `out` first, then by name.
    pub fn sort_outputs(&mut self) {
        self.outputs
            .sort_by(|a, b| (a.name != "out", &a.name).cmp(&(b.name != "out", &b.name)));
    }

    /// Check-run title and markdown summary for the build of `drv_path`.
    pub fn render(&self, drv_path: &str) -> (String, String) {
        let details = self.details_markdown(drv_path);
        let warnings: Vec<_> = self
            .warnings()
            .into_iter()
            .map(SizeWarningReport::from_warning)
            .collect();
        if warnings.is_empty() {
            return (self.success_title(), details);
        }
        let (title, warning_text) = size_warning::render(&warnings);
        (title, format!("{warning_text}\n\n---\n\n{details}"))
    }

    fn success_title(&self) -> String {
        let how = match (self.built_locally, self.duration_secs) {
            (true, Some(secs)) => format!("✅ Built in {}", format_duration(secs)),
            (true, None) => "✅ Built".to_string(),
            (false, _) => "✅ Substituted from cache".to_string(),
        };
        match self.total_closure_size {
            Some(size) => format!("{how} · closure {}", format_size(size)),
            None => how,
        }
    }

    fn details_markdown(&self, drv_path: &str) -> String {
        let mut sections = vec![self.overview_table(drv_path)];
        if !self.outputs.is_empty() {
            sections.push(self.outputs_table());
        }
        if !self.comparisons.is_empty() {
            sections.push(self.comparisons_table());
        }
        sections.join("\n\n")
    }

    fn overview_table(&self, drv_path: &str) -> String {
        let mut rows = vec!["| | |".to_string(), "|---|---|".to_string()];
        if let Some(package) = &self.package {
            rows.push(format!("| **Package** | {} |", escape_cell(package)));
        }
        if !self.system.is_empty() {
            rows.push(format!("| **System** | `{}` |", self.system));
        }
        rows.push(format!("| **Derivation** | `{drv_path}` |"));
        rows.push(format!("| **Build** | {} |", self.build_cell()));
        if let Some(size) = self.total_output_size {
            rows.push(format!("| **Output size** | {} |", format_size(size)));
        }
        if let Some(size) = self.total_closure_size {
            rows.push(format!("| **Closure size** | {} |", format_size(size)));
        }
        rows.join("\n")
    }

    fn build_cell(&self) -> String {
        match (self.built_locally, self.duration_secs) {
            (true, Some(secs)) => format!("Built in {}", format_duration(secs)),
            (true, None) => "Built".to_string(),
            (false, _) => "Substituted from a binary cache".to_string(),
        }
    }

    fn outputs_table(&self) -> String {
        let mut rows = vec![
            "### Outputs".to_string(),
            String::new(),
            "| Output | Store path | Size | Closure | Closure paths |".to_string(),
            "|---|---|--:|--:|--:|".to_string(),
        ];
        for output in &self.outputs {
            rows.push(format!(
                "| {} | `{}` | {} | {} | {} |",
                escape_cell(&output.name),
                output.path,
                output.nar_size.map_or("–".to_string(), format_size),
                output.closure_size.map_or("–".to_string(), format_size),
                output
                    .closure_paths
                    .map_or("–".to_string(), |n| n.to_string()),
            ));
        }
        rows.join("\n")
    }

    fn comparisons_table(&self) -> String {
        let mut rows = vec![
            "### Compared with base branch".to_string(),
            String::new(),
            "| Attr | Size | Baseline | This build | Change | Threshold |".to_string(),
            "|---|---|--:|--:|--:|--:|".to_string(),
        ];
        for c in &self.comparisons {
            let marker = if c.exceeded() { "⚠️ " } else { "" };
            rows.push(format!(
                "| `{}` | {}{} | {} | {} | {} ({:+.1}%) | {:.1}% |",
                c.attr,
                marker,
                metric_cell(c.metric, &c.base_branch),
                format_size(c.baseline_size),
                format_size(c.current_size),
                format_diff(c.baseline_size, c.current_size),
                c.increase_percent,
                c.threshold_percent,
            ));
        }
        rows.join("\n")
    }
}

/// `Output (master)`: the metric and the branch its baseline came from.
fn metric_cell(metric: SizeMetric, base_branch: &str) -> String {
    if base_branch.is_empty() {
        metric.label().to_string()
    } else {
        format!("{} (`{}`)", metric.label(), base_branch)
    }
}

/// Signed human-readable difference, e.g. `+1.50 KB` or `-200 bytes`.
fn format_diff(baseline: u64, current: u64) -> String {
    if current >= baseline {
        format!("+{}", format_size(current - baseline))
    } else {
        format!("-{}", format_size(baseline - current))
    }
}

/// Human-readable duration, e.g. `42s`, `3m 05s` or `1h 02m`.
pub(super) fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Keep a value from breaking out of its markdown table cell.
fn escape_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}
