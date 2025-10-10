use std::borrow::Cow;
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;

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
/// DrvId::try_from("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv")
/// ```
///
/// Derivation for the source of an unknown other derivation:
/// ```
/// DrvId::try_from("0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv")
/// ```
///
/// The `try_from` implementation strips any store paths from the input and enforces that the given
/// derivation identifier matches the `hash-name.drv` pattern. It also works with `&Path` inputs.
///
/// [^nix-by-hand]: <https://bernsteinbear.com/blog/nix-by-hand/>
#[derive(Clone, Debug, Hash, PartialEq, Eq, FromRow)]
pub struct DrvId(String);

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
}

impl std::str::FromStr for DrvId {
    type Err = anyhow::Error;

    fn from_str(drv_path: &str) -> Result<Self, anyhow::Error> {
        Ok(DrvId(strip_store_path(drv_path).to_string()))
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

impl TryFrom<&str> for DrvId {
    type Error = InvalidDrvId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // enforce that derivation identifiers have a `drv` extension
        match value.rsplit_once('.') {
            Some((_, "drv")) => {}, // all is well
            _ => return Err(InvalidDrvId),
        };

        // strip any potential store paths
        let id = strip_store_path(value);

        // enforce that derivation identifiers match the `hash-name` pattern
        match id.split_once('-') {
            Some((hash, _)) if hash.len() == 32 => {}, // all is well
            _ => return Err(InvalidDrvId),
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
}
