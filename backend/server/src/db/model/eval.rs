use super::ForInsert;
use crate::db::DbService;
use sqlx::FromRow;
use std::collections::HashMap;
use tracing::debug;

#[derive(Clone, Debug, FromRow)]
pub struct Drv {
    pub id: i64,

    /// Derivation store path
    pub drv_path: String,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,
}

impl Drv {
    /// Create a partially filled row
    pub fn for_insert(drv_path: String, system: String) -> ForInsert<Self> {
        ForInsert(Self {
            id: 1,
            drv_path,
            system,
        })
    }
}

impl DbService {
    pub async fn has_drv(&self, drv_path: &str) -> anyhow::Result<bool> {
        let result = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM Drv WHERE drv_path = $1)")
            .bind(drv_path)
            .fetch_one(&self.pool)
            .await?;
        Ok(result)
    }

    /// This will insert a hashmap of <drv, Vec<referrences>> into
    /// the database. The assumption is that the keys are new drvs and the
    /// references may or may not already exist
    pub async fn insert_drv_graph(
        &self,
        drv_graph: HashMap<String, Vec<String>>,
    ) -> anyhow::Result<()> {
        let mut reference_map: HashMap<i64, Vec<String>> = HashMap::new();
        // We must first traverse the keys, add them all, then we can create
        // the reference relationships
        for (drv, references) in drv_graph {
            debug!("Inserting {:?} into Drv", &drv);
            // TODO: have system be captured before this function
            let db_drv = self
                .insert_drv(Drv::for_insert(drv.to_string(), "x86_64-linux".to_string()))
                .await?;
            reference_map.insert(db_drv.id, references);
        }

        for (id, references) in reference_map {
            for reference in references {
                debug!("Inserting {:?},{:?} into DrvRef", id, &reference);
                self.insert_drv_ref(id, reference).await?;
            }
        }

        Ok(())
    }

    /// To avoid two round trips, or multiple subqueries, we assume that the referrer
    /// was recently inserted, thus we know its id. The references will be added
    /// by their drv_path since that was not yet known
    pub async fn insert_drv_ref(
        &self,
        drv_referrer_id: i64,
        drv_reference_path: String,
    ) -> anyhow::Result<()> {
        debug!(
            "Inserting DrvRef ({:?}, {:?})",
            drv_referrer_id, drv_reference_path
        );

        sqlx::query(
            r#"
INSERT INTO DrvRefs
    (referrer, reference)
VALUES (?1, (SELECT id from Drv WHERE drv_path = ?2))
        "#,
        )
        .bind(drv_referrer_id)
        .bind(drv_reference_path)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn insert_drv(&self, drv: ForInsert<Drv>) -> anyhow::Result<Drv> {
        let id: i64 = sqlx::query_scalar(
            r#"
INSERT INTO Drv
    (drv_path, system)
VALUES (?1, ?2)
RETURNING id
        "#,
        )
        .bind(&drv.0.drv_path)
        .bind(&drv.0.system)
        .fetch_one(&self.pool)
        .await?;

        Ok(Drv { id, ..drv.0 })
    }
}
