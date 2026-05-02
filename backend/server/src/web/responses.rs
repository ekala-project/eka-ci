// Response helper functions
//
// These collapse the verbose `(StatusCode::X, "msg").into_response()` pattern
// used throughout handlers into self-documenting calls, keeping handler
// happy-paths visually prominent.

use axum::response::IntoResponse;

pub(super) fn bad_request(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::BAD_REQUEST, msg.into()).into_response()
}

pub(super) fn not_found(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::NOT_FOUND, msg.into()).into_response()
}

pub(super) fn internal_error(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.into()).into_response()
}

pub(super) fn forbidden(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::FORBIDDEN, msg.into()).into_response()
}

pub(super) fn service_unavailable(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::SERVICE_UNAVAILABLE, msg.into()).into_response()
}
