mod approved_users;
mod github;
#[allow(dead_code, reason = "Only model definition for now, remove once used.")]
pub mod model;
mod service;

pub use approved_users::ApprovedUser;
pub use service::DbService;
