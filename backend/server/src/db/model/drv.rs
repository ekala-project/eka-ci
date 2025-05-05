use sqlx::encode::IsNull;
use sqlx::sqlite::SqliteArgumentValue;
use sqlx::{Decode, Encode, FromRow, Pool, Sqlite, Type};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;
use tracing::debug;

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
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
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
    /// Take a drv store path, and make it into a DrvId
    pub fn new(drv: String) -> Self {
        DrvId(drv.strip_prefix("/nix/store/").unwrap_or(&drv).to_string())
    }

    pub fn drv_id(&self) -> &str {
        &self.0
    }

    pub fn store_path(&self) -> String {
        format!("/nix/store/{}", self.0)
    }
}

/// Strips any store path from the input, if one exists.
fn strip_store_path(path: &str) -> &str {
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
            Some((_, "drv")) => {} // all is well
            _ => return Err(InvalidDrvId),
        };

        // strip any potential store paths
        let id = strip_store_path(value);

        // enforce that derivation identifiers match the `hash-name` pattern
        match id.split_once('-') {
            Some((hash, _)) if hash.len() == 32 => {} // all is well
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

#[derive(Debug, Clone, FromRow)]
pub struct Drv {
    /// Derivation identifier.
    pub derivation: DrvId,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,

    pub required_system_features: Option<String>,
}

impl Drv {
    /// Calls `nix derivation show` to retrieve system and requiredSystemFeatures
    pub async fn fetch_info(drv_path: &str) -> anyhow::Result<Self> {
        use crate::nix::derivation_show::drv_output;
        let drv_output = drv_output(drv_path).await?;
        Ok(Drv {
            derivation: DrvId::try_from(drv_path)?,
            system: drv_output.system,
            required_system_features: drv_output.required_system_features,
        })
    }
}

pub async fn has_drv(pool: &Pool<Sqlite>, drv_path: &str) -> anyhow::Result<bool> {
    let result = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM Drv WHERE drv_path = $1)")
        .bind(strip_store_path(drv_path))
        .fetch_one(pool)
        .await?;
    Ok(result)
}

/// This will insert a hashmap of <drv, Vec<referrences>> into
/// the database. The assumption is that the keys are new drvs and the
/// references may or may not already exist
pub async fn insert_drv_graph(
    pool: &Pool<Sqlite>,
    drv_graph: HashMap<DrvId, Vec<DrvId>>,
) -> anyhow::Result<()> {
    // We must first traverse the keys, add them all, then we can create
    // the reference relationships
    for id in drv_graph.keys() {
        debug!("Inserting {:?} into Drv", id);
        // TODO: have system be captured before this function
        let drv = Drv::fetch_info(&id.store_path()).await?;
        insert_drv(pool, &drv).await?;
    }

    for (referrer, references) in drv_graph {
        for reference in references {
            debug!("Inserting {:?},{:?} into DrvRef", &referrer, &reference);
            insert_drv_ref(pool, &referrer, &reference).await?;
        }
    }

    Ok(())
}

/// To avoid two round trips, or multiple subqueries, we assume that the referrer
/// was recently inserted, thus we know its id. The references will be added
/// by their drv_path since that was not yet known
pub async fn insert_drv_ref(
    pool: &Pool<Sqlite>,
    referrer_id: &DrvId,
    reference_id: &DrvId,
) -> anyhow::Result<()> {
    debug!("Inserting DrvRef ({:?}, {:?})", referrer_id, reference_id);

    sqlx::query(
        r#"
INSERT INTO DrvRefs
    (referrer, reference)
VALUES (?1, ?2)
    "#,
    )
    .bind(referrer_id)
    .bind(reference_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_drv(pool: &Pool<Sqlite>, drv: &Drv) -> anyhow::Result<()> {
    sqlx::query(
        r#"
INSERT INTO Drv
    (drv_path, system, required_system_features)
VALUES (?1, ?2, ?3)
    "#,
    )
    .bind(&drv.derivation)
    .bind(&drv.system)
    .bind(&drv.required_system_features)
    .execute(pool)
    .await?;

    Ok(())
}
