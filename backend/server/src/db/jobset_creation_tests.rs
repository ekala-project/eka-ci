use std::collections::{HashMap, HashSet};

use super::*;
use crate::nix::strategy::outputs_hash;

/// Build state of `Completed(Success)` in the `Drv` table.
const COMPLETED_SUCCESS: i64 = 42;

fn drv_path(hash_char: char, name: &str) -> String {
    format!("/nix/store/{}-{name}.drv", hash_char.to_string().repeat(32))
}

fn job(attr: &str, hash_char: char) -> NixEvalDrv {
    NixEvalDrv {
        attr: attr.to_string(),
        attr_path: vec![attr.to_string()],
        drv_path: drv_path(hash_char, attr),
        input_drvs: Some(HashMap::new()),
        name: attr.to_string(),
        outputs: HashMap::from([(
            "out".to_string(),
            format!("/nix/store/{}-{attr}", hash_char.to_string().repeat(32)),
        )]),
        system: "x86_64-linux".to_string(),
        meta: None,
        pos: None,
    }
}

fn jobset<'a>(
    sha: &'a str,
    jobs: &'a [NixEvalDrv],
    base_sha: Option<&'a str>,
    selected: Option<&'a HashSet<String>>,
) -> NewGitHubJobset<'a> {
    NewGitHubJobset {
        sha,
        name: "packages",
        owner: "owner",
        repo_name: "repo",
        jobs,
        config_json: None,
        base_sha,
        selected,
    }
}

/// A base with openssl, curl and hello; a head that rebuilds openssl and
/// curl and adds git, built with only openssl and git selected.
async fn selective_head(db: &DbService) -> anyhow::Result<i64> {
    let base = [job("openssl", 'a'), job("curl", 'b'), job("hello", 'c')];
    db.create_github_jobset(&jobset("base-sha", &base, None, None))
        .await?;

    let head = [
        job("openssl", 'd'),
        job("curl", 'f'),
        job("hello", 'c'),
        job("git", 'g'),
    ];
    let selected = HashSet::from(["openssl".to_string(), "git".to_string()]);
    db.create_github_jobset(&jobset(
        "head-sha",
        &head,
        Some("base-sha"),
        Some(&selected),
    ))
    .await
}

async fn set_state(db: &DbService, hash_char: char, name: &str, state: i64) {
    let path = drv_path(hash_char, name);
    let drv = path.trim_start_matches("/nix/store/");
    sqlx::query("UPDATE Drv SET build_state = ? WHERE drv_path = ?")
        .bind(state)
        .bind(drv)
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn skipped_jobs_are_not_dispatched_or_gated() -> anyhow::Result<()> {
    let db = DbService::new_in_memory().await?;
    let head = selective_head(&db).await?;

    let mut changed: Vec<String> = db
        .get_new_or_changed_jobs(head)
        .await?
        .into_iter()
        .map(|job| job.name)
        .collect();
    changed.sort();
    assert_eq!(changed, ["git", "openssl"]);

    assert!(!db.all_jobs_concluded(head).await?);
    set_state(&db, 'd', "openssl", COMPLETED_SUCCESS).await;
    set_state(&db, 'g', "git", COMPLETED_SUCCESS).await;
    // curl is skipped and still queued, but does not hold the jobset open.
    assert!(db.all_jobs_concluded(head).await?);
    Ok(())
}

#[tokio::test]
async fn jobset_records_output_hashes_and_base() -> anyhow::Result<()> {
    let db = DbService::new_in_memory().await?;
    selective_head(&db).await?;

    let head_rows = db.get_base_job_outputs("head-sha", "packages").await?;
    let curl = head_rows.iter().find(|row| row.name == "curl").unwrap();
    assert_eq!(
        curl.outputs_hash.as_deref(),
        Some(outputs_hash(&job("curl", 'f').outputs).as_str())
    );
    // Unchanged jobs get no row in a PR-head jobset.
    assert!(head_rows.iter().all(|row| row.name != "hello"));

    assert_eq!(
        db.get_base_job_outputs("base-sha", "packages").await?.len(),
        3
    );
    assert_eq!(
        db.get_jobset_base_sha("head-sha", "packages").await?,
        Some("base-sha".to_string())
    );
    assert_eq!(db.get_jobset_base_sha("base-sha", "packages").await?, None);
    assert_eq!(db.get_jobset_base_sha("missing", "packages").await?, None);
    Ok(())
}

#[tokio::test]
async fn jobset_drvs_are_not_traversed() -> anyhow::Result<()> {
    let db = DbService::new_in_memory().await?;
    selective_head(&db).await?;

    let curl = DrvId::from_str(&drv_path('f', "curl"))?;
    assert!(db.get_drv(&curl).await?.is_some());
    assert!(db.get_traversed_drv(&curl).await?.is_none());
    Ok(())
}

/// A docs-only PR (no job changes), or one whose rebuilt jobs are all
/// skipped, has nothing to build: its jobset is concluded as soon as it is
/// created, which is what `GitHubService::signal_if_concluded` checks.
#[tokio::test]
async fn jobset_with_nothing_to_build_concludes_at_creation() -> anyhow::Result<()> {
    let db = DbService::new_in_memory().await?;
    let base = [job("openssl", 'a'), job("hello", 'c')];
    db.create_github_jobset(&jobset("base-sha", &base, None, None))
        .await?;

    let nothing = HashSet::new();
    for (sha, selected) in [("docs-all", None), ("docs-selective", Some(&nothing))] {
        let docs = db
            .create_github_jobset(&jobset(sha, &base, Some("base-sha"), selected))
            .await?;
        assert!(db.all_jobs_concluded(docs).await?);
        assert!(!db.jobset_has_new_or_changed_failures(docs).await?);
    }

    let rebuilt = [job("openssl", 'd'), job("hello", 'c')];
    let skipped = db
        .create_github_jobset(&jobset(
            "bump-sha",
            &rebuilt,
            Some("base-sha"),
            Some(&nothing),
        ))
        .await?;
    assert!(db.all_jobs_concluded(skipped).await?);
    assert!(!db.jobset_has_new_or_changed_failures(skipped).await?);
    Ok(())
}
