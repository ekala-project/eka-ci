use anyhow::Result;
use sqlx::{Pool, Row, Sqlite};

use crate::nix::nix_eval_jobs::NixEvalDrv;
use super::model::Drv;

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

    // TODO: convert to using query builder
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

/// Select drvs which are present for a specific job
pub async fn jobs_for_jobset_id(
    job_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<Drv>> {
    let drvs = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.build_state
        FROM Drv d
        INNER JOIN Job j ON d.ROWID = j.drv_id
        WHERE j.jobset = ?
        "#,
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    Ok(drvs)
}

/// Select drvs which are only present in the head_sha
pub async fn new_jobs(
    head_sha: &str,
    base_sha: &str,
    job_name: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<Drv>> {
    use anyhow::Context;

    // First, get the jobset IDs for both head and base
    let head_jobset_id = sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
        .bind(head_sha)
        .bind(job_name)
        .fetch_optional(pool)
        .await?
        .context("Failed to find jobset for head sha")?;

    let maybe_base_jobset_id = sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
        .bind(base_sha)
        .bind(job_name)
        .fetch_optional(pool)
        .await?;

    // If there's no base jobset, treat all jobs as new values
    if let None = maybe_base_jobset_id {
        return jobs_for_jobset_id(head_jobset_id, pool).await;
    }
    let base_jobset_id: i64 = maybe_base_jobset_id.unwrap();

    // Query for drvs only present in the head jobset
    let drvs = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.build_state
        FROM Drv d
        INNER JOIN
        (SELECT drv_id
          FROM Job
          WHERE jobset = ?
          EXCEPT
          SELECT drv_id
          FROM Job
          WHERE jobset = ?
        ) j ON d.ROWID = j.drv_id
        "#
    )
        .bind(&head_jobset_id)
        .bind(&base_jobset_id)
        .fetch_all(pool)
        .await?;

    Ok(drvs)
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::model::Drv;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::insert_drv;
    use crate::db::model::drv_id::DrvId;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn create_github_jobs(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        let eval_drv_str = r#"{"attr":"grpc","attrPath":["grpc"],"drvPath":"/nix/store/qkgrb8v2ikxphb8raj8s0wd5rd7aip32-grpc-1.70.0.drv","name":"grpc-1.70.0","outputs":{"out":"/nix/store/rn3nlskr54yvw9gqq8im2g6c5bjyqqb5-grpc-1.70.0"},"system":"x86_64-linux"}"#;
        let eval_drv2_str = r#"{"attr":"grpc","attrPath":["grpc"],"drvPath":"/nix/store/ia82y2kxxnxh5jlzfbqahb2qqxrzh29b-grpc-1.75.0.drv","name":"grpc-1.75.0","outputs":{"out":"/nix/store/qjwvpghxiz69j35phbhpja0blfkfhjc5-grpc-1.75.0"},"system":"x86_64-linux"}"#;

        let eval_drv =
            serde_json::from_str::<NixEvalDrv>(eval_drv_str).expect("Failed to deserialize output");
        let eval_drv2 =
            serde_json::from_str::<NixEvalDrv>(eval_drv2_str).expect("Failed to deserialize output");

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
        create_jobs_for_jobset(jobset_id, jobs, &pool).await?;

        // These two queries should return the same result if there's no jobset associated with the
        // base commit
        let jobs = jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs.into_iter().next(), Some(drv.clone()));
        let newer_jobs = new_jobs("abcdef", "g1cdef", "fake-name", &pool).await?;
        assert_eq!(newer_jobs.len(), 1);
        assert_eq!(newer_jobs.into_iter().next(), Some(drv.clone()));

        // Create a second jobset, ensure we get just one result back
        let drv2 = Drv {
            drv_path: DrvId::from_str(&eval_drv2.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            build_state: DrvBuildState::Queued,
        };
        insert_drv(&pool, &drv2).await?;
        let jobs = &vec![("foo".to_string(), eval_drv2)];

        let second_jobset_id = create_jobset("g1cdef", "fake-name", &pool).await?;
        create_jobs_for_jobset(second_jobset_id, jobs, &pool).await?;
        let newer_jobs = new_jobs("abcdef", "g1cdef", "fake-name", &pool).await?;
        assert_eq!(newer_jobs.len(), 1);
        assert_eq!(newer_jobs.into_iter().next(), Some(drv));

        // If we reverse the commit order, we should get the other result
        let newer_jobs2: Vec<Drv> = new_jobs("g1cdef", "abcdef", "fake-name", &pool).await?;
        assert_eq!(newer_jobs2.len(), 1);
        assert_eq!(newer_jobs2.into_iter().next(), Some(drv2));

        Ok(())
    }
}
