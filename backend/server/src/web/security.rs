// Security utilities for web handlers

use axum::http::{HeaderValue, Method, header};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::warn;

/// Parse a `DrvId` from a string, returning a `(status, message)` pair on
/// failure that is suitable for `?`-propagation in handlers returning
/// `Result<_, (StatusCode, String)>`.
pub(super) fn parse_drv_id(
    drv: &str,
) -> Result<crate::db::model::drv_id::DrvId, (axum::http::StatusCode, String)> {
    crate::db::model::drv_id::DrvId::try_from(drv).map_err(|e| {
        warn!("Invalid drv format: {}", e);
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("Invalid derivation format: {}", e),
        )
    })
}

/// Defense-in-depth: verify `candidate` is `base` or a descendant of it.
/// Rejects `..`, absolute, or prefix components appended after `base` so
/// a future regression in upstream validation can't silently enable
/// directory traversal via `Path::join`.
pub(super) fn path_is_under(base: &std::path::Path, candidate: &std::path::Path) -> bool {
    use std::path::Component;
    if !candidate.starts_with(base) {
        return false;
    }
    let base_len = base.components().count();
    candidate
        .components()
        .skip(base_len)
        .all(|c| matches!(c, Component::Normal(_)))
}

/// M1: Build an explicit CORS layer from the configured allow-list.
///
/// - Origins are compared byte-for-byte against `HeaderValue`s derived from the configured list.
///   Entries that cannot be parsed as a header value are logged and silently skipped (the
///   allow-list is already validated at config load, so this should not fire in practice).
/// - Only `GET`, `POST`, and `OPTIONS` are permitted — this API does not currently expose any
///   cross-origin mutating verbs beyond `POST`.
/// - Only `authorization`, `content-type`, and `accept` are permitted on cross-origin requests.
/// - Credentialed CORS is explicitly NOT enabled: browsers will not attach cookies or
///   `Authorization` to cross-origin requests, so even an allow-listed origin cannot impersonate
///   the user.
pub(super) fn build_cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|origin| match HeaderValue::from_str(origin) {
            Ok(hv) => Some(hv),
            Err(e) => {
                warn!(
                    event = "cors_origin_unparseable",
                    origin = %origin,
                    error = %e,
                    "skipping CORS allow-list entry that could not be converted to a header value"
                );
                None
            },
        })
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT])
}

#[cfg(test)]
mod path_containment_tests {
    use std::path::PathBuf;

    use super::path_is_under;

    fn base() -> PathBuf {
        PathBuf::from("/var/lib/ekaci/logs")
    }

    #[test]
    fn accepts_base_itself() {
        assert!(path_is_under(&base(), &base()));
    }

    #[test]
    fn accepts_nested_normal_components() {
        let p = base().join("abcdef").join("build.log");
        assert!(path_is_under(&base(), &p));
    }

    #[test]
    fn rejects_parent_dir_escape_after_base() {
        // {base}/abc/../../etc/passwd lexically starts with base,
        // but the `..` component must still be rejected.
        let p = base().join("abc").join("..").join("..").join("etc");
        assert!(!path_is_under(&base(), &p));
    }

    #[test]
    fn rejects_sibling_prefix() {
        // /var/lib/ekaci/logs-evil does not start with /var/lib/ekaci/logs
        // at the component level.
        let p = PathBuf::from("/var/lib/ekaci/logs-evil/x");
        assert!(!path_is_under(&base(), &p));
    }

    #[test]
    fn rejects_absolute_component_after_base() {
        // Path::join with an absolute path replaces the prefix on Unix,
        // producing a candidate that does not start_with(base). Sanity
        // check that this too is rejected.
        let p = base().join("/etc/passwd");
        assert!(!path_is_under(&base(), &p));
    }

    #[test]
    fn rejects_parent_dir_as_first_suffix_component() {
        // {base}/../etc must be rejected: the first suffix
        // component attempts to escape upward.
        let p = base().join("..").join("etc");
        assert!(!path_is_under(&base(), &p));
    }

    #[test]
    fn rejects_candidate_disjoint_from_base() {
        let p = PathBuf::from("/etc/passwd");
        assert!(!path_is_under(&base(), &p));
    }
}
