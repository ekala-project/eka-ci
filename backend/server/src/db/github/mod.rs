// GitHub database operations module

pub mod auto_merge;
pub mod check_runs;
pub mod jobsets;
pub mod pull_requests;
pub mod repositories;
pub mod types;

// Re-export all public types
pub use auto_merge::*;
pub use check_runs::*;
pub use jobsets::*;
pub use pull_requests::*;
pub use repositories::*;
pub use types::*;

// Test helper functions (available for all tests in the crate)
#[cfg(test)]
pub(crate) mod test_helpers {
    use anyhow::Context;
    use sqlx::SqlitePool;

    use crate::db::model::Drv;

    /// Select drvs which are present for a specific job (test helper)
    pub(crate) async fn jobs_for_jobset_id(
        job_id: i64,
        pool: &SqlitePool,
    ) -> anyhow::Result<Vec<Drv>> {
        let drvs = sqlx::query_as(
            r#"
            SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
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

    /// Select drvs which are only present in the head_sha (test helper)
    pub(crate) async fn new_jobs(
        head_jobset_id: i64,
        base_jobset_id: i64,
        pool: &SqlitePool,
    ) -> anyhow::Result<Vec<Drv>> {
        // Query for drvs only present in the head jobset
        let new_drvs: Vec<Drv> = sqlx::query_as(
            r#"
            SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
            FROM Drv d
            INNER JOIN
            (SELECT drv_id
              FROM Job
              WHERE jobset = ?
              EXCEPT
              SELECT a.drv_id
              FROM Job AS a
              INNER JOIN Job AS b
              ON a.name = b.name
              WHERE a.jobset = ? AND b.jobset = ?
            ) j ON d.ROWID = j.drv_id
            "#,
        )
        .bind(head_jobset_id)
        .bind(head_jobset_id)
        .bind(base_jobset_id)
        .fetch_all(pool)
        .await?;

        Ok(new_drvs)
    }

    /// Select job names which are only present in the head_sha (test helper)
    pub(crate) async fn removed_jobs(
        head_jobset_id: i64,
        base_jobset_id: i64,
        pool: &SqlitePool,
    ) -> anyhow::Result<Vec<String>> {
        // Query for job names only present in the head jobset
        let removed_drvs: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT name
            FROM Job
            WHERE jobset = ?
            EXCEPT
            SELECT a.name
            FROM Job AS a
            INNER JOIN Job AS b
            ON a.name = b.name
            WHERE a.jobset = ? AND b.jobset = ?
            "#,
        )
        .bind(base_jobset_id)
        .bind(head_jobset_id)
        .bind(base_jobset_id)
        .fetch_all(pool)
        .await?;

        Ok(removed_drvs)
    }

    /// Compare two jobsets and return (new, changed, removed) drvs (test helper)
    pub(crate) async fn job_difference(
        head_sha: &str,
        base_sha: &str,
        job_name: &str,
        pool: &SqlitePool,
    ) -> anyhow::Result<(Vec<Drv>, Vec<Drv>, Vec<String>)> {
        // First, get the jobset IDs for both head and base
        let head_jobset_id =
            sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                .bind(head_sha)
                .bind(job_name)
                .fetch_optional(pool)
                .await?
                .context("Failed to find jobset for head sha")?;

        let maybe_base_jobset_id =
            sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                .bind(base_sha)
                .bind(job_name)
                .fetch_optional(pool)
                .await?;

        // If there's no base jobset, treat all jobs as new values
        if maybe_base_jobset_id.is_none() {
            let new_jobs = jobs_for_jobset_id(head_jobset_id, pool).await?;
            return Ok((new_jobs, Vec::new(), Vec::new()));
        }
        let base_jobset_id: i64 = maybe_base_jobset_id.unwrap();

        // Query for drvs which differ in drv_id but share the same job name
        let changed_drvs = sqlx::query_as(
            r#"
            SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
            FROM Drv d
            INNER JOIN
            (
              SELECT a.drv_id
              FROM Job AS a
              INNER JOIN Job AS b
              ON a.name = b.name
              WHERE a.jobset = ? AND b.jobset = ? AND a.drv_id <> b.drv_id
            ) j ON d.ROWID = j.drv_id
            "#,
        )
        .bind(head_jobset_id)
        .bind(base_jobset_id)
        .fetch_all(pool)
        .await?;

        let new_drvs = new_jobs(head_jobset_id, base_jobset_id, pool).await?;

        // Removed jobs are just "new" when you invert direction, however, we just need Job name
        // (not full Drv), since removed jobs don't exist in the head jobset.
        let removed_names = removed_jobs(head_jobset_id, base_jobset_id, pool).await?;

        Ok((new_drvs, changed_drvs, removed_names))
    }
}

#[cfg(test)]
mod tests {
    use sqlx::{Row, SqlitePool};

    use super::*;
    use crate::db::model::Drv;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::insert_drv;
    use crate::db::model::drv_id::DrvId;
    use crate::nix::nix_eval_jobs::NixEvalDrv;

    /// Test helper: insert a drv with a specific state
    async fn insert_drv_with_state(
        pool: &SqlitePool,
        drv_path: &str,
        state: DrvBuildState,
    ) -> anyhow::Result<()> {
        use std::str::FromStr;

        let drv = Drv {
            drv_path: DrvId::from_str(drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: state,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(pool, &drv).await?;
        Ok(())
    }

    /// Test helper: create a test NixEvalDrv
    fn eval_drv_for(attr: &str, drv_path: &str) -> NixEvalDrv {
        NixEvalDrv {
            attr: attr.to_string(),
            attr_path: vec![attr.to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: Default::default(),
            name: attr.to_string(),
            outputs: Default::default(),
            system: "x86_64-linux".to_string(),
            meta: None,
        }
    }

    /// Test helper: insert a test PR
    async fn insert_test_pr(
        pool: &SqlitePool,
        pr_number: i64,
        owner: &str,
        repo: &str,
        head_sha: &str,
    ) -> anyhow::Result<()> {
        upsert_pull_request(
            pr_number,
            owner,
            repo,
            head_sha,
            "base_sha",
            "Test PR",
            "test_author",
            "open",
            "2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00Z",
            pool,
        )
        .await?;
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn create_github_jobs(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        let eval_drv_str = r#"{"attr":"cmake","attrPath":["cmake"],"drvPath":"/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv","inputDrvs":{"/nix/store/08s4j5nvddsbrjpachqwzai83xngxnc0-pkg-config-wrapper-0.29.2.drv":["out"],"/nix/store/0cgbdlz63qiqf5f8i1sljak1dfbzyrl5-openssl-3.0.14.drv":["dev"],"/nix/store/265x0i426vnqjma9khcfpi86m6hx4smr-bash-5.2p32.drv":["out"],"/nix/store/27zlixdsk0kx585j4dcjm53636mx7cis-libuv-1.48.0.drv":["dev"],"/nix/store/2vyizsckka60lhh0kylhbpdd1flb998v-cmake-3.29.6.tar.gz.drv":["out"],"/nix/store/4hzjv6r5v7h6hzad718jgc0hrm1gz8r1-gcc-wrapper-13.3.0.drv":["out"],"/nix/store/860zddz386bk0441flrg940ipbp0jp1z-xz-5.6.2.drv":["dev"],"/nix/store/9jvlq6qg9j1222w3zm3wgfv5qyqfqmxz-bzip2-1.0.8.drv":["dev"],"/nix/store/ax4q30iyf9wi95hswil021lg0cdqq6rl-libarchive-3.7.4.drv":["dev"],"/nix/store/bxq3kjf71wn92yisdbq18fzpvcl5pn31-expat-2.6.2.drv":["dev"],"/nix/store/kh6mps96srqgdvn03vq4gmqzl51s9w8h-glibc-2.39-52.drv":["bin","dev","out"],"/nix/store/lzc503qcc7f6ibq8sdbcri73wb62dj4r-zlib-1.3.1.drv":["dev"],"/nix/store/mzw7jzs6ix17ajh3z4kqzvh8l7abj4yr-rhash-1.4.4.drv":["out"],"/nix/store/v288gxsg679gyi9zpg0mhrv26vfmw4kr-stdenv-linux.drv":["out"],"/nix/store/vnq47hr4nwry8kgvfgmx0229id3q49dr-binutils-2.42.drv":["out"],"/nix/store/y99v9h2mcqbw91g7p3lnk292k0np0djr-curl-8.9.0.drv":["dev"]},"name":"cmake-3.29.6","outputs":{"debug":"/nix/store/xrh9g28kmsyjlw6qf46ngkvhac1llgvz-cmake-3.29.6-debug","out":"/nix/store/rz7j0kdkq8j522vpw6n8wjq2qv3if24g-cmake-3.29.6"},"system":"x86_64-linux"}"#;

        let eval_drv =
            serde_json::from_str::<NixEvalDrv>(eval_drv_str).expect("Failed to deserialize output");
        let mut eval_drv2 = eval_drv.clone();
        eval_drv2.drv_path =
            "/nix/store/3fr8baalygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv".to_string();

        let drv = Drv {
            drv_path: DrvId::from_str(&eval_drv.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;
        let jobs = [eval_drv];

        println!("creating jobset");
        let jobset_id = create_jobset(
            "abcdef",
            "fake-name",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(jobset_id, &jobs[..], None, &pool).await?;

        // These two queries should return the same result if there's no jobset associated with the
        // base commit
        let jobs = test_helpers::jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs.into_iter().next(), Some(drv.clone()));

        // Create a second jobset, ensure we get just one result back
        let drv2 = Drv {
            drv_path: DrvId::from_str(&eval_drv2.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv2).await?;
        let jobs = [eval_drv2];

        let second_jobset_id = create_jobset(
            "g1cdef",
            "fake-name",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(second_jobset_id, &jobs[..], None, &pool).await?;
        let (_, changed_jobs, _) =
            test_helpers::job_difference("abcdef", "g1cdef", "fake-name", &pool).await?;
        assert_eq!(changed_jobs.len(), 1);
        assert_eq!(changed_jobs.into_iter().next(), Some(drv));

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_empty(pool: SqlitePool) -> anyhow::Result<()> {
        // Create a jobset
        let jobset_id = create_jobset(
            "test-sha",
            "test-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;

        // Call with empty jobs list - should not error
        let empty_jobs: Vec<NixEvalDrv> = vec![];
        create_jobs_for_jobset(jobset_id, &empty_jobs, None, &pool).await?;

        // Verify no jobs were created
        let jobs = test_helpers::jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 0);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_multiple_jobs_batch(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        use std::str::FromStr;

        // Create multiple test drvs and eval_drvs
        let drv_paths = vec![
            "/nix/store/1fr8b3xlygv2a64ff7fq7564j4sxv4lc-package1.drv",
            "/nix/store/2fr8b3xlygv2a64ff7fq7564j4sxv4lc-package2.drv",
            "/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-package3.drv",
        ];

        let mut eval_drvs = Vec::new();
        for (i, drv_path) in drv_paths.iter().enumerate() {
            let drv = Drv {
                drv_path: DrvId::from_str(drv_path)?,
                system: "x86_64-linux".to_string(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: None,
                version: None,
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            };
            insert_drv(&pool, &drv).await?;

            let attr = format!("package{}", i + 1);
            eval_drvs.push(eval_drv_for(&attr, drv_path));
        }

        // Create a jobset
        let jobset_id = create_jobset(
            "test-sha",
            "test-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;

        // Create jobs - this should batch them
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Verify all jobs were created
        let jobs = test_helpers::jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 3);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_verifies_relationships(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        use std::str::FromStr;

        // Create test drvs
        let drv1_path = "/nix/store/1fr8b3xlygv2a64ff7fq7564j4sxv4lc-package1.drv";
        let drv2_path = "/nix/store/2fr8b3xlygv2a64ff7fq7564j4sxv4lc-package2.drv";

        for drv_path in &[drv1_path, drv2_path] {
            let drv = Drv {
                drv_path: DrvId::from_str(drv_path)?,
                system: "x86_64-linux".to_string(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: None,
                version: None,
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            };
            insert_drv(&pool, &drv).await?;
        }

        let eval_drvs = vec![
            eval_drv_for("package1", drv1_path),
            eval_drv_for("package2", drv2_path),
        ];

        // Create a jobset
        let jobset_id = create_jobset(
            "test-sha",
            "test-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;

        // Create jobs
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Verify jobs are correctly linked
        let result = sqlx::query(
            "SELECT j.name, d.drv_path FROM Job j
             JOIN Drv d ON j.drv_id = d.ROWID
             WHERE j.jobset = ?
             ORDER BY j.name",
        )
        .bind(jobset_id)
        .fetch_all(&pool)
        .await?;

        use crate::db::model::drv_id::strip_store_path;

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].get::<String, _>("name"), "package1");
        assert_eq!(
            result[0].get::<String, _>("drv_path"),
            strip_store_path(drv1_path)
        );
        assert_eq!(result[1].get::<String, _>("name"), "package2");
        assert_eq!(
            result[1].get::<String, _>("drv_path"),
            strip_store_path(drv2_path)
        );

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_large_batch(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        // Create 100 test drvs
        let mut eval_drvs = Vec::new();
        for i in 0..100 {
            // Create a valid 32-character hash by padding i with leading zeros
            let hash = format!("{:02}r8b3xlygv2a64ff7fq7564j4sxv4lc", i);
            let drv_path = format!("/nix/store/{}-package{}.drv", hash, i);
            let drv = Drv {
                drv_path: DrvId::from_str(&drv_path)?,
                system: "x86_64-linux".to_string(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: None,
                version: None,
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            };
            insert_drv(&pool, &drv).await?;

            let attr = format!("package{}", i);
            eval_drvs.push(eval_drv_for(&attr, &drv_path));
        }

        // Create a jobset
        let jobset_id = create_jobset(
            "test-sha",
            "test-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;

        // Create jobs in batch
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Verify all jobs were created
        let jobs = test_helpers::jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 100);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_no_jobset_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Create a PR with no jobset
        insert_test_pr(&pool, 1, "owner", "repo", "sha123").await?;

        // Should return false since there's no jobset
        let result = pr_head_build_succeeded(1, "owner", "repo", &pool).await?;
        assert!(!result);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_non_terminal_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Create a PR
        insert_test_pr(&pool, 1, "owner", "repo", "sha123").await?;

        // Create a jobset for the PR's head SHA
        let jobset_id = create_jobset("sha123", "job1", "owner", "repo", None, &pool).await?;

        // Create a drv in a non-terminal state (Queued)
        insert_drv_with_state(
            &pool,
            "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-test1.drv",
            DrvBuildState::Queued,
        )
        .await?;

        let eval_drvs = vec![eval_drv_for(
            "test1",
            "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-test1.drv",
        )];
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Should return false since jobs haven't concluded
        let result = pr_head_build_succeeded(1, "owner", "repo", &pool).await?;
        assert!(!result);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_failure_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Create a PR
        insert_test_pr(&pool, 1, "owner", "repo", "sha123").await?;

        // Create a jobset for the PR's head SHA
        let jobset_id = create_jobset("sha123", "job1", "owner", "repo", None, &pool).await?;

        // Create a drv with a failure state
        insert_drv_with_state(
            &pool,
            "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-test1.drv",
            DrvBuildState::Completed(crate::db::model::build_event::DrvBuildResult::Failure),
        )
        .await?;

        let eval_drvs = vec![eval_drv_for(
            "test1",
            "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-test1.drv",
        )];
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Should return false since there's a failure
        let result = pr_head_build_succeeded(1, "owner", "repo", &pool).await?;
        assert!(!result);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_all_success_returns_true(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Create a PR
        insert_test_pr(&pool, 1, "owner", "repo", "sha123").await?;

        // Create a jobset for the PR's head SHA
        let jobset_id = create_jobset("sha123", "job1", "owner", "repo", None, &pool).await?;

        // Create a drv initially in Queued state
        let drv_path_str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-test1.drv";
        insert_drv_with_state(&pool, drv_path_str, DrvBuildState::Queued).await?;

        let eval_drvs = vec![eval_drv_for("test1", drv_path_str)];
        create_jobs_for_jobset(jobset_id, &eval_drvs, None, &pool).await?;

        // Update the drv to success state after job creation
        use std::str::FromStr;
        let drv_id = DrvId::from_str(drv_path_str)?;
        sqlx::query("UPDATE Drv SET build_state = ? WHERE drv_path = ?")
            .bind(42i64) // CompletedSuccess = 42
            .bind(&drv_id)
            .execute(&pool)
            .await?;

        // Should return true since all jobs succeeded
        let result = pr_head_build_succeeded(1, "owner", "repo", &pool).await?;
        assert!(result);

        Ok(())
    }
}
