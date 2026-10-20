// Search-index generation subsystem.
//
// Produces a SQLite database with FTS5 indexes (packages, files,
// options) after a release-channel promotion and uploads it to a
// configured storage destination. The ekapkgs CLI fetches this
// database for instant tab completion and search.

pub mod service;
pub mod types;
pub mod upload;

// Re-export generators from the extracted search_index crate.
pub use search_index::generators;
pub use service::SearchIndexService;
