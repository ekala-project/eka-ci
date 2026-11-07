pub mod jobs;

// Re-export key functions and types
pub use jobs::{
    ConsumeOutcome, EVAL_JOBS_MAX_ENTRIES, EVAL_JOBS_MAX_LINE_BYTES, EVAL_JOBS_MAX_STDOUT_BYTES,
    Truncation, process_eval_output, run_eval_jobs,
};
