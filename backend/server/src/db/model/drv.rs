use sqlx::{FromRow, Pool, Sqlite, Type};
use std::{collections::HashMap, fmt};
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
/// `jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv`
///
/// Derivation for the source of an unknown other derivation:
/// `0aykaqxhbby7mx7lgb217m9b3gkl52fn-source.drv`
///
/// [^nix-by-hand]: <https://bernsteinbear.com/blog/nix-by-hand/>
#[derive(Clone, Debug, Hash, PartialEq, Eq, Type)]
#[sqlx(transparent)]
pub struct DrvId(pub String);

/// Constructors and methods useful for testing.
#[cfg(test)]
impl DrvId {
    /// Returns a known good derivation identifier. Useful for database inserts in tests.
    pub fn dummy() -> Self {
        DrvId("jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv".to_owned())
    }
}

#[derive(Clone, FromRow)]
pub struct Drv {
    /// Derivation identifier.
    pub derivation: DrvId,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,
}

impl Drv {
    pub fn full_drv_path(&self) -> String {
        format!("/nix/store/{}", &self.derivation.0)
    }
}

impl fmt::Debug for Drv {
    // Allow for debug output to resemble expected output
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{{ drv_path:{}, system:{} }}",
            self.full_drv_path(),
            &self.system
        )
    }
}

pub fn strip_store_prefix(drv_path: &str) -> String {
    drv_path
        .strip_prefix("/nix/store/")
        .unwrap_or(&drv_path)
        .to_string()
}

pub async fn has_drv(pool: &Pool<Sqlite>, drv_path: &str) -> anyhow::Result<bool> {
    let result = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM Drv WHERE drv_path = $1)")
        .bind(strip_store_prefix(drv_path))
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
        insert_drv(pool, id, "x86_64-linux").await?;
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

pub async fn insert_drv(pool: &Pool<Sqlite>, id: &DrvId, system: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"
INSERT INTO Drv
    (drv_path, system)
VALUES (?1, ?2)
    "#,
    )
    .bind(id)
    .bind(system)
    .execute(pool)
    .await?;

    Ok(())
}
