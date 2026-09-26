// Search-index generation subsystem.
//
// Produces zstd-compressed JSON indexes (packages, files, options)
// after a release-channel promotion and uploads them to a configured
// storage destination. The ekapkgs CLI fetches these indexes for
// instant tab completion and search.

pub mod generators;
pub mod service;
pub mod types;
pub mod upload;

pub use service::SearchIndexService;
