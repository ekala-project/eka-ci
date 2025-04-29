use sqlx::{FromRow, Pool, Sqlite};
use std::collections::HashMap;
use tracing::debug;

#[derive(Clone, Debug, FromRow)]
pub struct Drv {
    /// Derivation store path
    pub drv_path: String,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,
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
    for (drv_path, references) in drv_graph {
        debug!("Inserting {:?} into Drv", &drv_path);
        // TODO: have system be captured before this function
        let drv = Drv {
            drv_path: drv_path.to_string(),
            system: "x86_64-linux".to_string(),
        };
        insert_drv(pool, &drv).await?;
        reference_map.insert(drv_path, references);
    }

    for (drv_path, references) in reference_map {
        for reference in references {
            debug!("Inserting {:?},{:?} into DrvRef", &drv_path, &reference);
            insert_drv_ref(pool, drv_path.clone(), reference).await?;
        }
    }

    Ok(())
}

/// To avoid two round trips, or multiple subqueries, we assume that the referrer
/// was recently inserted, thus we know its id. The references will be added
/// by their drv_path since that was not yet known
pub async fn insert_drv_ref(
    pool: &Pool<Sqlite>,
    drv_referrer_path: String,
    drv_reference_path: String,
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
