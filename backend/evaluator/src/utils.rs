/// Extract pname from a bare derivation name (no `/nix/store/<hash>-` prefix).
///
/// The `name` field of `nix-eval-jobs` output is normally `${pname}-${version}`
/// (e.g., `"hello-2.12.1"` or `"python3.12-setuptools-69.0.0"`). This function
/// strips trailing version-looking suffixes to extract the package name.
///
/// Returns the input unchanged if no version-looking suffix is found.
pub fn pname_from_name(name: &str) -> String {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 1 {
        return name.to_string();
    }
    let mut keep_parts = parts.len();
    for (i, part) in parts.iter().enumerate().rev() {
        if looks_like_version(part) {
            keep_parts = i;
        } else {
            break;
        }
    }
    if keep_parts == 0 {
        // All parts look like versions — keep everything (defensive).
        name.to_string()
    } else {
        parts[..keep_parts].join("-")
    }
}

/// Extract version from a bare derivation name. Returns `None` if there's no
/// trailing version-looking suffix, or if the entire name is version-like.
pub fn version_from_name(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 1 {
        return None;
    }
    let mut first_version_idx = parts.len();
    for (i, part) in parts.iter().enumerate().rev() {
        if looks_like_version(part) {
            first_version_idx = i;
        } else {
            break;
        }
    }
    if first_version_idx == 0 || first_version_idx == parts.len() {
        None
    } else {
        Some(parts[first_version_idx..].join("-"))
    }
}

/// Check if a string segment looks like a version identifier
fn looks_like_version(s: &str) -> bool {
    // Empty or very short strings are not versions
    if s.len() < 2 {
        return false;
    }

    // Starts with 'v' or 'r' followed by digit (v1.2.3, r1, etc.)
    if s.len() > 1 {
        let first_char = s.chars().next().unwrap();
        let second_char = s.chars().nth(1).unwrap();
        if (first_char == 'v' || first_char == 'r') && second_char.is_ascii_digit() {
            return true;
        }
    }

    // Contains only digits and dots (1.2.3, 2024.01.01, etc.)
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let all_digits_and_dots = s.chars().all(|c| c.is_ascii_digit() || c == '.');

    if has_digit && all_digits_and_dots {
        return true;
    }

    // Starts with digit (common for versions like "3.12", "2024")
    if s.chars().next().unwrap().is_ascii_digit() {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pname_from_name() {
        assert_eq!(pname_from_name("hello-2.12.1"), "hello");
        assert_eq!(
            pname_from_name("python3.12-setuptools-69.0.0"),
            "python3.12-setuptools"
        );
        assert_eq!(pname_from_name("source"), "source");
        assert_eq!(pname_from_name("rust-analyzer-v0.3.1234"), "rust-analyzer");
        assert_eq!(
            pname_from_name("nixpkgs-unstable-2024.01.01"),
            "nixpkgs-unstable"
        );
        // No trailing version - returned as-is
        assert_eq!(pname_from_name("just-a-name"), "just-a-name");
        // Empty
        assert_eq!(pname_from_name(""), "");
    }

    #[test]
    fn test_version_from_name() {
        assert_eq!(
            version_from_name("hello-2.12.1"),
            Some("2.12.1".to_string())
        );
        assert_eq!(
            version_from_name("python3.12-setuptools-69.0.0"),
            Some("69.0.0".to_string())
        );
        assert_eq!(version_from_name("source"), None);
        assert_eq!(
            version_from_name("rust-analyzer-v0.3.1234"),
            Some("v0.3.1234".to_string())
        );
        assert_eq!(
            version_from_name("nixpkgs-unstable-2024.01.01"),
            Some("2024.01.01".to_string())
        );
        // Multi-segment trailing version
        assert_eq!(
            version_from_name("foo-1.2.3-r1"),
            Some("1.2.3-r1".to_string())
        );
        // No version-looking suffix
        assert_eq!(version_from_name("just-a-name"), None);
    }

    #[test]
    fn test_looks_like_version() {
        assert!(looks_like_version("1.2.3"));
        assert!(looks_like_version("2024.01.01"));
        assert!(looks_like_version("v1.2.3"));
        assert!(looks_like_version("r1"));
        assert!(looks_like_version("69.0.0"));
        assert!(looks_like_version("3.12"));

        assert!(!looks_like_version("hello"));
        assert!(!looks_like_version("python3"));
        assert!(!looks_like_version("setuptools"));
        assert!(!looks_like_version("x"));
        assert!(!looks_like_version(""));
    }
}
