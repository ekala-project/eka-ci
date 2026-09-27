// Search-index generation subsystem.
//
// Produces a SQLite database with FTS5 indexes (packages, files,
// options) after a release-channel promotion and uploads it to a
// configured storage destination. The ekapkgs CLI fetches this
// database for instant tab completion and search.

pub mod generators;
pub mod service;
pub mod types;
pub mod upload;

pub use service::SearchIndexService;
