pub mod jobs;

// Re-export key functions and types
pub use jobs::{
    ConsumeOutcome, NIX_EVAL_JOBS_MAX_ENTRIES, NIX_EVAL_JOBS_MAX_LINE_BYTES,
    NIX_EVAL_JOBS_MAX_STDOUT_BYTES, Truncation, process_nix_eval_output, run_nix_eval_jobs,
};
