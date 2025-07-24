use crate::db::model::build_event::DrvBuildState;
use crate::db::model::DrvId;
use sqlx::SqlitePool;
use sqlx::{FromRow, Pool, Sqlite};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::debug;

#[derive(Debug, Clone, FromRow)]
pub struct Drv {
    /// Derivation identifier.
    pub drv_path: DrvId,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,

    pub required_system_features: Option<String>,

    /// This is None when queried from Nix
    /// Otherwise, it is the latest build status
    pub build_state: Option<DrvBuildState>,
}

impl Drv {
    /// Calls `nix derivation show` to retrieve system and requiredSystemFeatures
    pub async fn fetch_info(drv_path: &str) -> anyhow::Result<Self> {
        use crate::nix::derivation_show::drv_output;
        let drv_output = drv_output(drv_path).await?;
        Ok(Drv {
            drv_path: DrvId::from_str(drv_path)?,
            system: drv_output.system,
            required_system_features: drv_output.required_system_features,
            build_state: None,
        })
    }
}

pub async fn get_drv(derivation: &DrvId, pool: &Pool<Sqlite>) -> anyhow::Result<Option<Drv>> {
    let event = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, build_state
FROM Drv
WHERE drv_path = ?
        "#,
    )
    .bind(derivation)
    .fetch_optional(pool)
    .await?;

    Ok(event)
}

pub async fn has_drv(pool: &Pool<Sqlite>, drv_path: &str) -> anyhow::Result<bool> {
    use super::drv_id::strip_store_path;

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
    drv_graph: &HashMap<DrvId, Vec<DrvId>>,
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
    (drv_path, system, required_system_features, build_state)
VALUES (?1, ?2, ?3, ?4)
    "#,
    )
    .bind(&drv.drv_path)
    .bind(&drv.system)
    .bind(&drv.required_system_features)
    .bind(DrvBuildState::Queued)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn update_drv_status(
    pool: &Pool<Sqlite>,
    drv_id: &DrvId,
    build_state: &DrvBuildState,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
UPDATE Drv
SET build_state = ?1
WHERE drv_path = ?2
    "#,
    )
    .bind(&build_state)
    .bind(&drv_id)
    .execute(pool)
    .await?;

    // TODO: emit build_event in the same transaction
    Ok(())
}

async fn insert_drvs(categories: Vec<Drv>, pool: &Pool<Sqlite>) -> anyhow::Result<()> {
    use sqlx::QueryBuilder;

    let mut query_builder = QueryBuilder::new(
        "INSERT INTO Drv (drv_path, system, required_system_features, build_state) ",
    );

    query_builder.push_values(categories, |mut row, drv| {
        row.push_bind(drv.drv_path)
            .push_bind(drv.system)
            .push_bind(drv.required_system_features)
            .push_bind(drv.build_state);
    });

    query_builder.build().execute(pool).await?;

    Ok(())
}

/// "Upstream" drvs, dependencies.
pub async fn drv_references(pool: &Pool<Sqlite>, drv: &DrvId) -> anyhow::Result<Vec<Drv>> {
    let result = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, build_state
FROM Drv
JOIN DrvRefs ON Drv.drv_path = DrvRefs.reference
WHERE referrer = ?1
"#,
    )
    .bind(&drv)
    .fetch_all(pool)
    .await?;

    Ok(result)
}

/// "Downstream" drvs, consumers.
pub async fn drv_referrers(pool: &Pool<Sqlite>, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
    let result = sqlx::query_as(
        r#"
SELECT referrer
FROM DrvRefs
WHERE reference = ?1
"#,
    )
    .bind(&drv)
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn get_derivations_in_state(
    state: DrvBuildState,
    pool: &SqlitePool,
) -> anyhow::Result<Vec<DrvId>> {
    let res = sqlx::query_as(
        r#"
SELECT drv_path
FROM Drv
WHERE build_state = ?
        "#,
    )
    .bind(state)
    .fetch_all(pool)
    .await?;

    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    // use crate::db::model::DrvId;
    // use crate::db::model::drv::DrvBuildState;
    // use super::insert_drv;
    use anyhow::bail;
    // use futures::StreamExt;
    use sqlx::SqlitePool;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn get_latest_state(pool: SqlitePool) -> anyhow::Result<()> {
        let drv_id = DrvId::dummy();
        let drv = Drv {
            drv_path: drv_id.clone(),
            system: "x86_64-linux".to_string(),
            required_system_features: None,
            build_state: Some(DrvBuildState::Queued),
        };
        println!("inserting drv");
        insert_drv(&pool, &drv).await?;
        update_drv_status(&pool, &drv_id, &DrvBuildState::Buildable).await?;

        println!("querying for drv");
        let Some(result) = get_drv(&drv_id, &pool).await? else {
            bail!("Expected query to find a result")
        };

        assert_eq!(result.build_state, Some(DrvBuildState::Buildable));
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn insert_many(pool: SqlitePool) -> anyhow::Result<()> {
        let drv1 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.1.drv",
            )?,
            system: "x86_64-linux".to_string(),
            required_system_features: None,
            build_state: Some(DrvBuildState::Queued),
        };
        let drv2 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/p470qfnbrf16agb4r05fllbsqgi2m8k5-git-2.47.2.drv",
            )?,
            system: "x86_64-linux".to_string(),
            required_system_features: None,
            build_state: Some(DrvBuildState::Queued),
        };
        let drv3 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/0wy8117gx1hbdv85x2xq1vf12nlagan4-bash-interactive-5.2p37.drv",
            )?,
            system: "x86_64-linux".to_string(),
            required_system_features: None,
            build_state: Some(DrvBuildState::Buildable),
        };

        let drvs = vec![drv1, drv2, drv3];

        insert_drvs(drvs, &pool).await?;

        let result = get_derivations_in_state(DrvBuildState::Queued, &pool).await?;

        for drv in &result {
            debug!("{:?}", &drv);
        }

        // TODO: make less ugly
        let length = result.len();
        assert_eq!(length, 2);
        Ok(())
    }
}
