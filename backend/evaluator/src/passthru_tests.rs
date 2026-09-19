//! Generate a Nix expression that evaluates `passthru.tests` for a set of
//! changed attribute paths.
//!
//! The generated file is fed to `nix-eval-jobs` to discover test derivations
//! for packages that were directly modified in a PR.

use std::fmt::Write;
use std::io::Write as IoWrite;

use anyhow::{Context, Result};

/// Generate a temporary Nix file that evaluates `passthru.tests` for each
/// attribute in `changed_attrs`.
///
/// The generated expression imports `original_file` and for each attr uses
/// `builtins.tryEval` so packages without `passthru.tests` gracefully
/// evaluate to `{}` (producing zero test derivations).
///
/// Returns a `NamedTempFile` whose path can be passed to `nix-eval-jobs`.
/// The caller must keep the handle alive until evaluation completes.
pub fn generate_passthru_tests_expr(
    original_file: &str,
    changed_attrs: &[String],
) -> Result<tempfile::NamedTempFile> {
    let nix_src = build_nix_source(original_file, changed_attrs);

    let mut tmp = tempfile::Builder::new()
        .prefix("ekaci-passthru-tests-")
        .suffix(".nix")
        .tempfile()
        .context("failed to create temp file for passthru.tests expression")?;

    tmp.write_all(nix_src.as_bytes())
        .context("failed to write passthru.tests expression")?;

    Ok(tmp)
}

/// Build the Nix source string. Separated from I/O for testability.
fn build_nix_source(original_file: &str, changed_attrs: &[String]) -> String {
    let mut nix = String::with_capacity(256 + changed_attrs.len() * 80);

    // Header: import the original file and define a safe accessor.
    //
    // `safeTests` uses `tryEval` twice:
    //   1. to access the package itself (may throw if the attr is aliased/removed)
    //   2. to access `passthru.tests` (may not exist)
    //
    // If either step fails the attr evaluates to `{}` and nix-eval-jobs
    // produces zero derivations for it.
    let _ = writeln!(nix, "let");
    let _ = writeln!(nix, "  pkgs = import {} {{}};", original_file);
    let _ = writeln!(nix, "  safeTests = path:");
    let _ = writeln!(nix, "    let");
    let _ = writeln!(
        nix,
        "      pkg = builtins.tryEval (builtins.deepSeq path path);"
    );
    let _ = writeln!(nix, "    in");
    let _ = writeln!(nix, "      if !pkg.success then {{}}");
    let _ = writeln!(nix, "      else");
    let _ = writeln!(
        nix,
        "        let tests = builtins.tryEval (pkg.value.passthru.tests or {{}});"
    );
    let _ = writeln!(
        nix,
        "        in if tests.success then tests.value else {{}};"
    );
    let _ = writeln!(nix, "in {{");

    for attr in changed_attrs {
        let accessor = attr_to_nix_accessor(attr);
        // Each attr path becomes a top-level key whose value is the
        // test set (or {} if the package lacks passthru.tests).
        let _ = writeln!(nix, "  \"{}\" = safeTests pkgs.{};", attr, accessor);
    }

    let _ = writeln!(nix, "}}");
    nix
}

/// Convert a dotted attr path like `python.pkgs.setuptools` into
/// a Nix accessor chain: `python.pkgs.setuptools`.
///
/// Simple attrs like `hello` pass through unchanged.
fn attr_to_nix_accessor(attr: &str) -> String {
    // Attr paths from nix-eval-jobs are already dot-separated and safe
    // (they consist of Nix identifiers). We split and rejoin with dots
    // to validate the structure, but the result is the same string for
    // well-formed inputs.
    attr.split('.')
        .map(|component| {
            // If a component contains characters that aren't valid bare
            // Nix identifiers, quote it. In practice nix-eval-jobs attrs
            // are always bare identifiers, but this is defensive.
            if needs_quoting(component) {
                format!("\"{}\"", component)
            } else {
                component.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Returns true if a Nix identifier component needs quoting.
fn needs_quoting(component: &str) -> bool {
    if component.is_empty() {
        return true;
    }
    let first = component.as_bytes()[0];
    // Nix identifiers start with [a-zA-Z_] and contain [a-zA-Z0-9_'-]
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return true;
    }
    !component
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'\'' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_nix_source_simple_attr() {
        let src = build_nix_source("/path/to/release.nix", &["hello".to_string()]);
        assert!(src.contains("import /path/to/release.nix {}"));
        assert!(src.contains("\"hello\" = safeTests pkgs.hello;"));
        assert!(src.contains("safeTests = path:"));
    }

    #[test]
    fn build_nix_source_nested_attr() {
        let src = build_nix_source(
            "/nixpkgs/release.nix",
            &["python3.pkgs.setuptools".to_string()],
        );
        assert!(
            src.contains("\"python3.pkgs.setuptools\" = safeTests pkgs.python3.pkgs.setuptools;")
        );
    }

    #[test]
    fn build_nix_source_multiple_attrs() {
        let src = build_nix_source(
            "/f.nix",
            &[
                "hello".to_string(),
                "curl".to_string(),
                "python3.pkgs.requests".to_string(),
            ],
        );
        assert!(src.contains("\"hello\" = safeTests pkgs.hello;"));
        assert!(src.contains("\"curl\" = safeTests pkgs.curl;"));
        assert!(src.contains("\"python3.pkgs.requests\" = safeTests pkgs.python3.pkgs.requests;"));
    }

    #[test]
    fn build_nix_source_empty_attrs() {
        let src = build_nix_source("/f.nix", &[]);
        // Should still be valid Nix: `let ... in {}`
        assert!(src.contains("in {"));
        assert!(src.contains("}"));
    }

    #[test]
    fn attr_to_nix_accessor_simple() {
        assert_eq!(attr_to_nix_accessor("hello"), "hello");
    }

    #[test]
    fn attr_to_nix_accessor_nested() {
        assert_eq!(
            attr_to_nix_accessor("python3.pkgs.setuptools"),
            "python3.pkgs.setuptools"
        );
    }

    #[test]
    fn attr_to_nix_accessor_component_needing_quoting() {
        assert_eq!(attr_to_nix_accessor("123foo"), "\"123foo\"");
    }

    #[test]
    fn needs_quoting_bare_identifier() {
        assert!(!needs_quoting("hello"));
        assert!(!needs_quoting("_private"));
        assert!(!needs_quoting("my-pkg"));
        assert!(!needs_quoting("foo'bar"));
    }

    #[test]
    fn needs_quoting_special_cases() {
        assert!(needs_quoting(""));
        assert!(needs_quoting("123"));
        assert!(needs_quoting("foo.bar")); // dot is not valid in a bare component
    }

    #[test]
    fn generate_passthru_tests_expr_creates_valid_file() {
        let tmp = generate_passthru_tests_expr("/f.nix", &["hello".to_string()])
            .expect("should create temp file");
        let contents = std::fs::read_to_string(tmp.path()).expect("should read temp file");
        assert!(contents.contains("\"hello\" = safeTests pkgs.hello;"));
    }
}
