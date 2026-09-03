mod builder;
mod builder_thread;
mod queue;
mod system_queue;

pub use builder::*;
pub use queue::*;
pub use system_queue::*;

pub type Platform = String;

/// Snapshot of available builder feature sets, shared with the ingress
/// service so it can detect unsatisfiable feature requirements early
/// (before the drv reaches the build queue).
#[derive(Clone, Debug)]
pub struct BuilderFeatureSnapshot {
    /// Each entry is (supported_features, mandatory_features) for one builder.
    builders: Vec<(Vec<String>, Vec<String>)>,
}

impl BuilderFeatureSnapshot {
    pub fn new(builders: Vec<(Vec<String>, Vec<String>)>) -> Self {
        Self { builders }
    }

    /// Check if at least one builder can handle a drv with the given
    /// required system features.
    pub fn can_build(&self, required_features: &Option<String>) -> bool {
        let required: Vec<String> = required_features
            .as_ref()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default();

        if required.is_empty() {
            return true;
        }

        self.builders.iter().any(|(supported, mandatory)| {
            // If builder has mandatory features, drv must require at least one
            if !mandatory.is_empty() && !required.iter().any(|req| mandatory.contains(req)) {
                return false;
            }
            required.iter().all(|req| supported.contains(req))
        })
    }
}
