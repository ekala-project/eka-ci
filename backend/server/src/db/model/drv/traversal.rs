//! Lookups that distinguish traversed drvs from eval-only rows.
//!
//! Evaluation writes a `Drv` row for every job so `Job` rows can reference
//! it, but only traversal records a drv's closure and edges. Eval-only rows
//! (`traversed = 0`) must not be mistaken for traversed ones: they are never
//! loaded into the graph and their build info is a placeholder.

use sqlx::{Pool, Sqlite};

use super::Drv;
use crate::db::model::DrvId;

/// Fetch a drv only if it has been traversed into the dependency graph.
pub async fn get_traversed_drv(drv: &DrvId, pool: &Pool<Sqlite>) -> anyhow::Result<Option<Drv>> {
    let row = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, is_fod, build_state, output_size, closure_size,
       pname, version, license_json, maintainers_json, meta_position, broken, insecure
FROM Drv
WHERE drv_path = ? AND traversed = 1
        "#,
    )
    .bind(drv)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::SqlitePool;

    use super::*;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::insert_eval_only_drvs;
    use crate::db::model::drv::{get_all_drvs, get_drv, insert_drvs_and_references};

    fn drv(hash_name: &str, features: Option<&str>, is_fod: bool) -> Drv {
        Drv {
            drv_path: DrvId::from_str(hash_name).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: features.map(str::to_string),
            is_fod,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: Some("hello".to_string()),
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }

    const A: &str = "jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";
    const B: &str = "aaaal3jn2mkn530lgcg0y523jq5qji85-world-1.0.drv";

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn eval_only_rows_are_not_traversed(pool: SqlitePool) -> anyhow::Result<()> {
        insert_eval_only_drvs(&pool, &[drv(A, None, false)]).await?;
        insert_drvs_and_references(&pool, &[drv(B, None, false)], &[]).await?;

        let a = DrvId::from_str(A)?;
        assert!(get_drv(&a, &pool).await?.is_some());
        assert!(get_traversed_drv(&a, &pool).await?.is_none());

        let loaded: Vec<String> = get_all_drvs(&pool)
            .await?
            .into_iter()
            .map(|d| d.drv_path.to_string())
            .collect();
        assert_eq!(loaded, vec![B.to_string()]);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn traversal_upgrades_eval_only_placeholder(pool: SqlitePool) -> anyhow::Result<()> {
        insert_eval_only_drvs(&pool, &[drv(A, None, false)]).await?;
        insert_drvs_and_references(&pool, &[drv(A, Some("kvm nixos-test"), true)], &[]).await?;

        let a = DrvId::from_str(A)?;
        let row = get_traversed_drv(&a, &pool)
            .await?
            .expect("traversal marks the row traversed");
        assert_eq!(
            row.required_system_features.as_deref(),
            Some("kvm nixos-test")
        );
        assert!(row.is_fod);
        assert_eq!(row.pname.as_deref(), Some("hello"));
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn eval_only_insert_keeps_traversed_row(pool: SqlitePool) -> anyhow::Result<()> {
        insert_drvs_and_references(&pool, &[drv(A, Some("kvm"), false)], &[]).await?;
        insert_eval_only_drvs(&pool, &[drv(A, None, false)]).await?;

        let a = DrvId::from_str(A)?;
        let row = get_traversed_drv(&a, &pool)
            .await?
            .expect("an eval-only insert never un-traverses a row");
        assert_eq!(row.required_system_features.as_deref(), Some("kvm"));
        Ok(())
    }
}
