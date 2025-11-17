use anyhow::Result;
use sqlx::{Pool, Row, Sqlite};

use crate::nix::nix_eval_jobs::NixEvalDrv;

pub async fn create_jobset(sha: &str, name: &str, pool: &Pool<Sqlite>) -> Result<i64> {
    let result = sqlx::query("INSERT INTO GitHubJobSets (sha, job) VALUES (?, ?) RETURNING ROWID")
        .bind(sha)
        .bind(name)
        .fetch_one(pool)
        .await?;

    let rowid: i64 = result.get("ROWID");
    Ok(rowid)
}

/// Insert jobs where they reference the job and the drv
pub async fn create_jobs_for_jobset(
    jobset_id: i64,
    jobs: &Vec<(String, NixEvalDrv)>,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    use std::str::FromStr;

    use crate::db::model::DrvId;

    for (name, job) in jobs {
        // Convert drv_path to DrvId
        let drv_id = DrvId::from_str(&job.drv_path)?;

        // Insert into DrvRefs table
        sqlx::query(
            "INSERT INTO Job (jobset, drv_id, name) VALUES (?, (SELECT rowid FROM Drv WHERE \
             drv_path = ? LIMIT 1), ?)",
        )
        .bind(jobset_id)
        .bind(drv_id)
        .bind(&name)
        .execute(pool)
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::bail;
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::model::Drv;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::{get_drv, insert_drv};
    use crate::db::model::drv_id::DrvId;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn create_github_jobs(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        let eval_drv = r#"{"attr":"grpc","attrPath":["grpc"],"drvPath":"/nix/store/qkgrb8v2ikxphb8raj8s0wd5rd7aip32-grpc-1.70.0.drv","name":"grpc-1.70.0","outputs":{"out":"/nix/store/rn3nlskr54yvw9gqq8im2g6c5bjyqqb5-grpc-1.70.0"},"system":"x86_64-linux"}"#;

        let eval_drv =
            serde_json::from_str::<NixEvalDrv>(eval_drv).expect("Failed to deserialize output");

        let drv = Drv {
            drv_path: DrvId::from_str(&eval_drv.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            build_state: DrvBuildState::Queued,
        };
        insert_drv(&pool, &drv).await?;
        let jobs = &vec![("foo".to_string(), eval_drv)];

        println!("creating jobset");
        let jobset_id = create_jobset("abcdef", "fake-name", &pool).await?;
        println!("jobset_id: {}", &jobset_id);
        create_jobs_for_jobset(jobset_id, jobs, &pool).await?;

        Ok(())
    }
}
