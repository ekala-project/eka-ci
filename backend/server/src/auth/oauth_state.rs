//! CSRF `state` binding and post-login redirects for the GitHub OAuth flow.
//!
//! `/github/auth/login` stores the random `state` it sends to GitHub in a
//! short-lived HttpOnly cookie. `/github/auth/callback` only accepts a
//! `state` that matches that cookie, so an attacker cannot complete their
//! own authorization in a victim's browser (login CSRF).
//!
//! The callback is reached by a full browser navigation from GitHub, so it
//! answers with a redirect to the frontend's `/auth/complete` page. The
//! result travels in the URL fragment, which browsers never send to servers
//! or proxies, keeping the JWT out of access logs.

use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Name of the cookie holding the pending OAuth `state`.
pub(super) const STATE_COOKIE: &str = "ekaci_oauth_state";

/// Cookie path: only sent to the login/callback endpoints.
const STATE_COOKIE_PATH: &str = "/github/auth";

/// How long a login attempt may take before the state expires.
const STATE_COOKIE_MAX_AGE_SECS: u32 = 600;

/// Frontend page that finishes the login.
pub(super) const COMPLETE_PATH: &str = "/auth/complete";

/// `Set-Cookie` value storing `state` for the callback to verify.
///
/// `SameSite=Lax` still sends the cookie on GitHub's top-level GET
/// redirect back to the callback. `Secure` is set whenever the configured
/// redirect URL is HTTPS.
pub(super) fn set_state_cookie(state: &str, secure: bool) -> String {
    format!(
        "{STATE_COOKIE}={state}; Path={STATE_COOKIE_PATH}; Max-Age={STATE_COOKIE_MAX_AGE_SECS}; \
         HttpOnly; SameSite=Lax{}",
        secure_attr(secure)
    )
}

/// `Set-Cookie` value deleting the state cookie (each state is single-use).
pub(super) fn clear_state_cookie(secure: bool) -> String {
    format!(
        "{STATE_COOKIE}=; Path={STATE_COOKIE_PATH}; Max-Age=0; HttpOnly; SameSite=Lax{}",
        secure_attr(secure)
    )
}

fn secure_attr(secure: bool) -> &'static str {
    if secure { "; Secure" } else { "" }
}

/// Read the pending state from the request's `Cookie` headers.
pub(super) fn read_state_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == STATE_COOKIE)
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty())
}

/// Whether the callback's `state` matches the cookie, in constant time.
pub(super) fn state_matches(expected: Option<&str>, received: Option<&str>) -> bool {
    let (Some(expected), Some(received)) = (expected, received) else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), received.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Redirect to the frontend with the login result in the fragment, and
/// clear the state cookie.
pub(super) fn complete_redirect(fragment: &str, secure: bool) -> Response {
    let location = format!("{COMPLETE_PATH}#{fragment}");
    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    match HeaderValue::from_str(&location) {
        Ok(value) => {
            headers.insert(LOCATION, value);
        },
        Err(e) => {
            tracing::error!("OAuth redirect location is not a valid header: {}", e);
            headers.insert(LOCATION, HeaderValue::from_static(COMPLETE_PATH));
        },
    }
    if let Ok(cookie) = HeaderValue::from_str(&clear_state_cookie(secure)) {
        headers.insert(SET_COOKIE, cookie);
    }
    response
}

/// Fragment carrying a successful login's JWT.
pub(super) fn token_fragment(token: &str) -> String {
    format!("token={}", encode(token))
}

/// Fragment carrying a user-facing login error.
pub(super) fn error_fragment(message: &str) -> String {
    format!("error={}", encode(message))
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_cookie(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(cookie).unwrap());
        headers
    }

    #[test]
    fn reads_state_among_other_cookies() {
        let headers = headers_with_cookie("a=1; ekaci_oauth_state=abc123; b=2");
        assert_eq!(read_state_cookie(&headers).as_deref(), Some("abc123"));
    }

    #[test]
    fn missing_or_empty_state_cookie_is_none() {
        assert_eq!(read_state_cookie(&HeaderMap::new()), None);
        let headers = headers_with_cookie("ekaci_oauth_state=");
        assert_eq!(read_state_cookie(&headers), None);
    }

    #[test]
    fn state_must_be_present_and_equal() {
        assert!(state_matches(Some("abc"), Some("abc")));
        assert!(!state_matches(Some("abc"), Some("abd")));
        assert!(!state_matches(Some("abc"), Some("abcd")));
        assert!(!state_matches(None, Some("abc")));
        assert!(!state_matches(Some("abc"), None));
        assert!(!state_matches(None, None));
    }

    #[test]
    fn cookie_attributes() {
        let cookie = set_state_cookie("xyz", true);
        assert!(cookie.starts_with("ekaci_oauth_state=xyz;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.ends_with("; Secure"));
        assert!(!set_state_cookie("xyz", false).contains("Secure"));
        assert!(clear_state_cookie(false).contains("Max-Age=0"));
    }

    #[test]
    fn redirect_puts_result_in_fragment_and_clears_cookie() {
        let response = complete_redirect(&error_fragment("bad state & stuff"), false);
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()[LOCATION].to_str().unwrap();
        assert_eq!(location, "/auth/complete#error=bad+state+%26+stuff");
        let cookie = response.headers()[SET_COOKIE].to_str().unwrap();
        assert!(cookie.contains("Max-Age=0"));
    }
}
