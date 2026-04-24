//! Secret-redacting newtype used to keep secrets out of logs.
//!
//! This addresses finding M2 ("Secrets may be written to logs at
//! debug level") by wrapping secret values in a [`Redacted<T>`]
//! newtype whose [`Debug`] and [`Display`] impls print
//! `"[REDACTED]"` instead of the inner value.
//!
//! # Design
//!
//! - `Redacted<T>` has no `Deref` impl: reads must go through [`Redacted::expose`] or
//!   [`Redacted::into_inner`] so that every access point is visible in review.
//! - Serialisation is `#[serde(transparent)]`: a `Redacted<String>` round-trips to/from JSON/TOML
//!   as the bare string, so types that legitimately ship a token to the client (e.g.
//!   `LoginResponse`) keep working while still redacting in logs.
//! - `PartialEq` / `Eq` forward to `T`, so tests can assert secret values when constructed
//!   explicitly.
//!
//! # Usage
//!
//! ```ignore
//! use crate::secret::Redacted;
//!
//! let s = Redacted::new("hunter2".to_string());
//! assert_eq!(format!("{:?}", s), "[REDACTED]");
//! assert_eq!(format!("{}", s), "[REDACTED]");
//! assert_eq!(s.expose(), "hunter2");
//! ```
use std::fmt;

use serde::{Deserialize, Serialize};

/// A wrapper that prevents the inner value from leaking through
/// [`Debug`] and [`Display`] formatting.
///
/// Construct with [`Redacted::new`]; read with [`Redacted::expose`]
/// or [`Redacted::into_inner`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

#[allow(dead_code)]
impl<T> Redacted<T> {
    /// Wrap a value as a redacted secret.
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    /// Borrow the inner secret. Prefer this over [`Redacted::into_inner`]
    /// so the compiler can track the shortest possible lifetime for
    /// the unwrapped secret.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwrap the secret by value. Use sparingly — prefer
    /// [`Redacted::expose`].
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> From<T> for Redacted<T> {
    fn from(inner: T) -> Self {
        Self(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts() {
        let r = Redacted::new("super-secret-token".to_string());
        assert_eq!(format!("{r:?}"), "[REDACTED]");
        assert_eq!(format!("{r:#?}"), "[REDACTED]");
    }

    #[test]
    fn display_redacts() {
        let r = Redacted::new("super-secret-token".to_string());
        assert_eq!(format!("{r}"), "[REDACTED]");
    }

    #[test]
    fn debug_redacts_when_nested_in_another_debug_struct() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Wrapper {
            name: &'static str,
            secret: Redacted<String>,
        }

        let w = Wrapper {
            name: "dev",
            secret: Redacted::new("hunter2".to_string()),
        };
        let out = format!("{w:?}");
        assert!(
            !out.contains("hunter2"),
            "nested Debug output leaked secret: {out}"
        );
        assert!(out.contains("[REDACTED]"), "missing marker: {out}");
    }

    #[test]
    fn debug_redacts_inside_vec() {
        let v = vec![
            Redacted::new("one".to_string()),
            Redacted::new("two".to_string()),
        ];
        let out = format!("{v:?}");
        assert!(!out.contains("one"), "leaked first secret: {out}");
        assert!(!out.contains("two"), "leaked second secret: {out}");
    }

    #[test]
    fn expose_returns_inner() {
        let r = Redacted::new("plaintext".to_string());
        assert_eq!(r.expose(), "plaintext");
        assert_eq!(r.into_inner(), "plaintext");
    }

    #[test]
    fn eq_forwards_to_inner() {
        let a = Redacted::new("same".to_string());
        let b = Redacted::new("same".to_string());
        let c = Redacted::new("other".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn serde_is_transparent() {
        let r: Redacted<String> = Redacted::new("wire-token".to_string());
        let json = serde_json::to_string(&r).expect("serialize");
        assert_eq!(json, "\"wire-token\"", "should serialize as bare string");

        let back: Redacted<String> = serde_json::from_str("\"wire-token\"").expect("deserialize");
        assert_eq!(back.expose(), "wire-token");
    }

    #[test]
    fn serde_transparent_in_struct() {
        #[derive(Serialize, Deserialize)]
        struct Envelope {
            token: Redacted<String>,
        }
        let e = Envelope {
            token: Redacted::new("abc".to_string()),
        };
        let json = serde_json::to_string(&e).expect("serialize");
        assert_eq!(json, "{\"token\":\"abc\"}");

        let back: Envelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.token.expose(), "abc");
    }

    #[test]
    fn from_impl_is_available() {
        let r: Redacted<String> = "secret".to_string().into();
        assert_eq!(format!("{r:?}"), "[REDACTED]");
        assert_eq!(r.expose(), "secret");
    }
}
