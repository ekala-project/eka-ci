use sqlx::{FromRow, Pool, Sqlite};
use std::collections::HashMap;
use std::fmt;
use tracing::debug;

#[derive(Clone, FromRow)]
pub struct Drv {
    /// Derivation store path
    pub drv_path: String,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,
}

impl Drv {
    pub fn new(drv_path: String, system: String) -> Self {
        Drv {
            drv_path: strip_store_prefix(drv_path),
            system,
        }
    }

    pub fn full_drv_path(&self) -> String {
        format!("/nix/store/{}", &self.drv_path)
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

pub fn strip_store_prefix(drv_path: String) -> String {
    drv_path
        .strip_prefix("/nix/store/")
        .unwrap_or(&drv_path)
        .to_string()
}

pub async fn has_drv(pool: &Pool<Sqlite>, drv_path: &str) -> anyhow::Result<bool> {
    let result = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM Drv WHERE drv_path = $1)")
        .bind(drv_path)
        .fetch_one(pool)
        .await?;
    Ok(result)
}

/// This will insert a hashmap of <drv, Vec<referrences>> into
/// the database. The assumption is that the keys are new drvs and the
/// references may or may not already exist
pub async fn insert_drv_graph(
    pool: &Pool<Sqlite>,
    drv_graph: HashMap<String, Vec<String>>,
) -> anyhow::Result<()> {
    let mut reference_map: HashMap<String, Vec<String>> = HashMap::new();
    // We must first traverse the keys, add them all, then we can create
    // the reference relationships
    for (drv_path, references) in &drv_graph {
        debug!("Inserting {:?} into Drv", &drv_path);
        // TODO: have system be captured before this function
        let drv = Drv::new(drv_path.to_string(), "x86_64-linux".to_string());
        insert_drv(pool, &drv).await?;
        reference_map.insert(drv.drv_path, references.clone());
    }

    for (drv_path, references) in reference_map {
        for reference in references {
            let fixed_reference = strip_store_prefix(reference);
            debug!(
                "Inserting {:?},{:?} into DrvRef",
                &drv_path, &fixed_reference
            );
            insert_drv_ref(pool, &drv_path, &fixed_reference).await?;
        }
    }

    Ok(())
}

/// To avoid two round trips, or multiple subqueries, we assume that the referrer
/// was recently inserted, thus we know its id. The references will be added
/// by their drv_path since that was not yet known
pub async fn insert_drv_ref(
    pool: &Pool<Sqlite>,
    drv_referrer_path: &str,
    drv_reference_path: &str,
) -> anyhow::Result<()> {
    debug!(
        "Inserting DrvRef ({:?}, {:?})",
        drv_referrer_path, drv_reference_path
    );

    sqlx::query(
        r#"
INSERT INTO DrvRefs
    (referrer, reference)
VALUES (?1, ?2)
    "#,
    )
    .bind(drv_referrer_path)
    .bind(drv_reference_path)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_drv(pool: &Pool<Sqlite>, drv: &Drv) -> anyhow::Result<()> {
    sqlx::query(
        r#"
INSERT INTO Drv
    (drv_path, system)
VALUES (?1, ?2)
    "#,
    )
    .bind(&drv.drv_path)
    .bind(&drv.system)
    .execute(pool)
    .await?;

    Ok(())
}
