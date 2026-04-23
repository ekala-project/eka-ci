use std::borrow::Cow;
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use sqlx::encode::IsNull;
use sqlx::sqlite::SqliteArgumentValue;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};

/// A derivation identifier of the form `hash-name.drv`.
///
/// Many derivations that describe a package (binaries, libraries, ...) additionally include a
/// version identifier in the name component. For these derivations, the identifier often looks
/// like `hash-name-version.drv`. This is however only a convention. Many intermediate build
/// artifacts for example do not have a version.
///
/// Each derivation identifier corresponds to a file with the same name located in a nix store. The
/// filesystem path of the store depends on the evaluator that produced the derivation and is part
/// of the identifier's hash component[^nix-by-hand]. It is not possible to determine the store
/// path given only a derivation identifier.
///
/// # Examples
///
/// Derivation for the hello package, version 2.12.1:
/// ```
/// use eka_ci_server::db::model::DrvId;
/// let id = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv").unwrap();
/// ```
///
/// Derivation for the source of an unknown other derivation:
/// ```
/// use eka_ci_server::db::model::DrvId;
/// let id = DrvId::try_from("0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv").unwrap();
/// ```
///
/// The `try_from` implementation strips any store paths from the input and enforces that the given
/// derivation identifier matches the `hash-name.drv` pattern. It also works with `&Path` inputs.
///
/// [^nix-by-hand]: <https://bernsteinbear.com/blog/nix-by-hand/>
#[derive(Clone, Debug, Hash, PartialEq, Eq, FromRow, Serialize)]
pub struct DrvId(String);

pub type Referrer = DrvId;
pub type Reference = DrvId;

// This type is really just a string, that we enforce to be of a certain structure, as such, it
// makes sense that all read-only methods on str should be available on this type as well.
impl Deref for DrvId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// The double reference is a bit weird, but this is the correct way of implementing `PartialEq` for
// comparison with strings, because it is the only impl that allows `drvid == "asdf"` to compile.
impl PartialEq<&str> for DrvId {
    fn eq(&self, other: &&str) -> bool {
        str::eq(self, *other)
    }
}

/// Helper implementations.
impl DrvId {
    pub fn store_path(&self) -> String {
        format!("/nix/store/{}", self.0)
    }

    /// Extract the hash component (first 32 characters) of the derivation identifier
    pub fn drv_hash(&self) -> &str {
        // DrvId is guaranteed to have format hash-name.drv where hash is 32 chars
        &self.0[..32]
    }

    /// (&self, reference) pairs, for easy inserting into DB
    pub async fn reference_pairs(&self) -> Result<Vec<(Referrer, Reference)>> {
        use std::str::FromStr;

        use tracing::warn;

        use crate::nix::drv_references;

        let refs = match drv_references(&self.store_path()).await {
            Err(e) => {
                warn!(
                    "Failed to fetch references for {}: {:?}",
                    self.store_path(),
                    e
                );
                vec![]
            },
            Ok(x) => x,
        };

        let pairs = refs
            .into_iter()
            .flat_map(|x| DrvId::from_str(&x).map(|y| (self.clone(), y)))
            .collect();

        Ok(pairs)
    }
}

impl std::str::FromStr for DrvId {
    type Err = InvalidDrvId;

    fn from_str(drv_path: &str) -> Result<Self, InvalidDrvId> {
        DrvId::try_from(drv_path)
    }
}

/// Strips any store path from the input, if one exists.
pub fn strip_store_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((_, file_name)) => file_name,
        None => path,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid derivation identifier")]
pub struct InvalidDrvId;

impl TryFrom<&Path> for DrvId {
    type Error = InvalidDrvId;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        let file_name = value
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or(InvalidDrvId)?;

        file_name.try_into()
    }
}

/// Nix's custom base32 alphabet, used to encode the 160-bit store-path
/// hash into the 32-character prefix of `hash-name.drv`.
/// Source: `src/libutil/hash.cc` in the nix source tree. The alphabet
/// omits `e`, `o`, `t`, `u` to reduce the chance of accidental
/// profanity in generated names.
const NIX_BASE32_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Upper bound on the length of a store-path basename (`hash-name.drv`).
/// Nix itself enforces a 211-char cap on the full basename; anything
/// longer is either user error or an injection attempt. Keeps
/// `DrvId::try_from` O(len) with a tight constant.
const DRV_ID_MAX_LEN: usize = 211;

/// Returns true iff the byte is a valid character of Nix's store-path
/// name component. The allowed set per the Nix reference manual is
/// `[A-Za-z0-9+\-._?=]`. This explicitly excludes path separators
/// (`/`, `\`), whitespace, control characters (< 0x20 or 0x7F),
/// quoting characters, shell metacharacters beyond the allowed set,
/// and every multi-byte UTF-8 sequence (all non-ASCII bytes).
#[inline]
fn is_valid_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
}

impl TryFrom<&str> for DrvId {
    type Error = InvalidDrvId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // strip any potential store paths first so the length / content
        // checks apply to the basename only.
        let id = strip_store_path(value);

        // upper-bound the length before doing per-byte work. The empty
        // string and anything longer than `DRV_ID_MAX_LEN` cannot be a
        // valid derivation identifier.
        if id.is_empty() || id.len() > DRV_ID_MAX_LEN {
            return Err(InvalidDrvId);
        }

        // enforce the `.drv` extension on the basename.
        let stem = match id.rsplit_once('.') {
            Some((stem, "drv")) => stem,
            _ => return Err(InvalidDrvId),
        };

        // split into (hash, name) — both non-empty.
        let (hash, name) = match stem.split_once('-') {
            Some((h, n)) if !h.is_empty() && !n.is_empty() => (h, n),
            _ => return Err(InvalidDrvId),
        };

        // hash must be exactly 32 characters from the nix base32
        // alphabet. Prevents traversal-via-hash-component and makes
        // `drv_hash()`'s fixed-32 slice safe on any `DrvId`.
        if hash.len() != 32 {
            return Err(InvalidDrvId);
        }
        if !hash.bytes().all(|b| NIX_BASE32_ALPHABET.contains(&b)) {
            return Err(InvalidDrvId);
        }

        // name component must be ASCII and only contain the characters
        // Nix accepts in store-path names. This rejects path separators
        // (`/`, `\`), control characters, whitespace, quoting and shell
        // metacharacters, and any multi-byte UTF-8 sequence.
        if !name.bytes().all(is_valid_name_byte) {
            return Err(InvalidDrvId);
        }

        Ok(Self(id.to_owned()))
    }
}

impl<'q> Encode<'q, Sqlite> for DrvId {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<IsNull, sqlx::error::BoxDynError> {
        // Using `Cow::Borrowed` is not possible because &self does not necessarily outlive 'q.
        buf.push(SqliteArgumentValue::Text(Cow::Owned(self.0.clone())));
        Ok(IsNull::No)
    }

    // Avoid the clone is `encode_by_ref` by overwriting the default impl of this method.
    fn encode(
        self,
        buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<IsNull, sqlx::error::BoxDynError>
    where
        Self: Sized,
    {
        buf.push(SqliteArgumentValue::Text(Cow::Owned(self.0)));
        Ok(IsNull::No)
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl<'r> Decode<'r, Sqlite> for DrvId {
    fn decode(
        value: <Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(<&str as Decode<'r, Sqlite>>::decode(value)?.try_into()?)
    }
}

impl Type<Sqlite> for DrvId {
    fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
        <&str as Type<Sqlite>>::type_info()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::DrvId;

    /// Constructors and methods useful for testing.
    impl DrvId {
        /// Returns a known good derivation identifier. Useful for database inserts in tests.
        pub fn dummy() -> Self {
            DrvId("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv".to_owned())
        }
    }

    #[test]
    fn drv_id_try_from() {
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv").unwrap();
        let _ = DrvId::try_from("0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv").unwrap();

        assert_eq!(
            DrvId::try_from(Path::new(
                "/nix/store/0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv",
            ))
            .unwrap(),
            "0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv"
        );
        assert_eq!(
            DrvId::try_from(Path::new(
                "/some/other/path/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv"
            ))
            .unwrap(),
            "jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv"
        );

        let _ = DrvId::try_from("").unwrap_err();
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1").unwrap_err();
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85.drv").unwrap_err();
        let _ = DrvId::try_from("hello-2.12.1.drv").unwrap_err();
    }

    /// Hash component must be exactly 32 chars from nix's custom base32
    /// alphabet. Anything with `e`, `o`, `t`, `u`, uppercase letters, or
    /// non-ASCII bytes must fail.
    #[test]
    fn drv_id_rejects_invalid_hash_alphabet() {
        // 'e' is not in nix base32
        let _ = DrvId::try_from("ed83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap_err();
        // 'o' is not in nix base32
        let _ = DrvId::try_from("od83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap_err();
        // 't' is not in nix base32
        let _ = DrvId::try_from("td83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap_err();
        // 'u' is not in nix base32
        let _ = DrvId::try_from("ud83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap_err();
        // uppercase hash rejected
        let _ = DrvId::try_from("JD83L3JN2MKN530LGCG0Y523JQ5QJI85-hello.drv").unwrap_err();
    }

    /// Hash must be exactly 32 chars; shorter or longer stems must fail.
    #[test]
    fn drv_id_rejects_hash_length_off_by_one() {
        // 31 chars (too short)
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji8-hello.drv").unwrap_err();
        // 33 chars (too long)
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji855-hello.drv").unwrap_err();
        // 32 chars, valid base32 - should succeed
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-x.drv").unwrap();
    }

    /// Name component must only contain characters from `[A-Za-z0-9+\-._?=]`.
    /// Slashes, backslashes, whitespace, control chars, shell metachars,
    /// and non-ASCII bytes must all be rejected.
    #[test]
    fn drv_id_rejects_invalid_name_bytes() {
        // forward slash in name - path-traversal concern
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel/lo.drv").unwrap_err();
        // backslash
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel\\lo.drv").unwrap_err();
        // space
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel lo.drv").unwrap_err();
        // tab
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel\tlo.drv").unwrap_err();
        // newline
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel\nlo.drv").unwrap_err();
        // null
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel\0lo.drv").unwrap_err();
        // single quote (shell injection concern)
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel'lo.drv").unwrap_err();
        // double quote
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel\"lo.drv").unwrap_err();
        // semicolon
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel;lo.drv").unwrap_err();
        // backtick
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel`lo.drv").unwrap_err();
        // dollar
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hel$lo.drv").unwrap_err();
        // non-ASCII (utf-8)
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-helló.drv").unwrap_err();
    }

    /// The characters explicitly allowed by the nix reference manual in
    /// store-path names: `[A-Za-z0-9+\-._?=]`.
    #[test]
    fn drv_id_accepts_full_name_alphabet() {
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-A-z.0+_=?.drv").unwrap();
    }

    /// Nix's 211-char basename cap must be enforced so overly-long inputs
    /// from an attacker cannot bloat memory or slow validation.
    #[test]
    fn drv_id_rejects_overlong_basename() {
        // name is 211 - 32 (hash) - 1 ('-') - 4 (".drv") = 174 valid chars
        let ok_name: String = "a".repeat(174);
        let ok = format!("jd83l3jn2mkn530lgcg0y523jq5qji85-{ok_name}.drv");
        assert_eq!(ok.len(), 211);
        let _ = DrvId::try_from(ok.as_str()).unwrap();

        // one byte past the cap
        let bad_name: String = "a".repeat(175);
        let bad = format!("jd83l3jn2mkn530lgcg0y523jq5qji85-{bad_name}.drv");
        assert_eq!(bad.len(), 212);
        let _ = DrvId::try_from(bad.as_str()).unwrap_err();
    }

    /// A leading or trailing `-` in the stem produces an empty hash or name
    /// component - both must be rejected.
    #[test]
    fn drv_id_rejects_empty_components() {
        // empty hash
        let _ = DrvId::try_from("-name.drv").unwrap_err();
        // empty name
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-.drv").unwrap_err();
        // stem with no hyphen at all
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85x.drv").unwrap_err();
    }

    /// The `.drv` extension is enforced case-sensitively - anything else
    /// must be rejected.
    #[test]
    fn drv_id_rejects_wrong_extension() {
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello.DRV").unwrap_err();
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drvv").unwrap_err();
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello.txt").unwrap_err();
        let _ = DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello").unwrap_err();
    }

    /// `strip_store_path` only keeps the last path segment, so path-traversal
    /// style inputs are reduced to their basename before validation.
    #[test]
    fn drv_id_strips_store_path_prefixes() {
        let got = DrvId::try_from("/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap();
        assert_eq!(&*got, "jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drv");

        // even oddly-nested inputs collapse to the basename
        let got = DrvId::try_from("../../jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drv").unwrap();
        assert_eq!(&*got, "jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drv");
    }

    /// `FromStr` must delegate to the same validation as `TryFrom<&str>`;
    /// it must not accept anything `try_from` rejects.
    #[test]
    fn drv_id_from_str_matches_try_from() {
        use std::str::FromStr;

        let inputs = [
            "jd83l3jn2mkn530lgcg0y523jq5qji85-hello.drv",
            "ed83l3jn2mkn530lgcg0y523jq5qji85-hello.drv", // invalid (e)
            "jd83l3jn2mkn530lgcg0y523jq5qji85-he/llo.drv", // invalid (/)
            "",
            "jd83l3jn2mkn530lgcg0y523jq5qji85.drv",
        ];

        for input in inputs {
            assert_eq!(
                DrvId::from_str(input).is_ok(),
                DrvId::try_from(input).is_ok(),
                "divergent validation for {input:?}"
            );
        }
    }
}
