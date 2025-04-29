use sqlx::SqlitePool;

use super::model::{
    build::{DrvBuildEvent, DrvBuildMetadata},
    ForInsert,
};

pub async fn new_drv_build_metadata(
    metadata: ForInsert<DrvBuildMetadata>,
    pool: &SqlitePool,
) -> anyhow::Result<DrvBuildMetadata> {
    let metadata = metadata.0;
    let metadata = sqlx::query_as(
        r#"
INSERT INTO DrvBuildMetadata
    (derivation, git_repo, git_commit, build_command, build_attempt)
VALUES (
    ?1, ?2, ?3, ?4,
    IFNULL(
        (SELECT MAX(build_attempt) + 1
            FROM DrvBuildMetadata
            WHERE derivation = ?1),
        1
    )
)
RETURNING derivation, build_attempt, git_repo, git_commit, build_command
        "#,
    )
    .bind(&metadata.build.derivation)
    .bind(&metadata.git_repo)
    .bind(&metadata.git_commit)
    .bind(&metadata.build_command)
    .fetch_one(pool)
    .await?;

    Ok(metadata)
}

pub async fn new_drv_build_event(
    event: ForInsert<DrvBuildEvent>,
    pool: &SqlitePool,
) -> anyhow::Result<DrvBuildEvent> {
    let event = event.0;
    let event = sqlx::query_as(
        r#"
INSERT INTO DrvBuildEvent
    (derivation, build_attempt, state)
VALUES (?1, ?2, ?3)
RETURNING derivation, build_attempt, state, timestamp
        "#,
    )
    .bind(&event.build.derivation)
    .bind(event.build.build_attempt)
    .bind(&event.state)
    .fetch_one(pool)
    .await?;

    Ok(event)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use crate::db::model::{
        build::{DrvBuildCommand, DrvBuildId, DrvBuildResult, DrvBuildState, DrvId},
        git::{GitCommit, GitRepo},
    };

    use super::*;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn insert_metadata_new_drv(pool: SqlitePool) -> anyhow::Result<()> {
        let metadata = DrvBuildMetadata::for_insert(
            DrvId::dummy(),
            GitRepo(gix_url::parse(
                "https://github.com/ekala-project/eka-ci".into(),
            )?),
            GitCommit(gix_hash::ObjectId::from_hex(
                b"ad7fb3f7660de7435baf14af66edef106dcffff9",
            )?),
            DrvBuildCommand::dummy(),
        );

        let inserted = new_drv_build_metadata(metadata.clone(), &pool).await?;

        // some sanity checks that the correct metadata record is returned
        assert_eq!(&inserted.build.derivation, &metadata.0.build.derivation);
        assert_eq!(&inserted.git_repo, &metadata.0.git_repo);
        assert_eq!(&inserted.git_commit, &metadata.0.git_commit);
        assert_eq!(&inserted.build_command, &metadata.0.build_command);

        // check that the build attempt was assigned correctly
        assert_eq!(inserted.build.build_attempt.get(), 1u32);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn insert_metadata_existing_drv(pool: SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO DrvBuildMetadata (derivation, build_attempt, git_repo, git_commit, build_command)
VALUES (?, 1, 'https://github.com/ekala-project/corepkgs', '0000000000000000000000000000000000000000', ?)
            "#,
        )
        .bind(DrvId::dummy())
        .bind(DrvBuildCommand::dummy())
        .execute(&pool)
        .await?;

        let metadata = DrvBuildMetadata::for_insert(
            DrvId::dummy(),
            GitRepo(gix_url::parse(
                "https://github.com/ekala-project/corepkgs".into(),
            )?),
            GitCommit(gix_hash::ObjectId::from_hex(
                b"1111111111111111111111111111111111111111",
            )?),
            DrvBuildCommand::dummy(),
        );

        // should create a new entry with build attempt counter set to 2 because another entry
        // for the same derivation already exists
        let inserted = new_drv_build_metadata(metadata.clone(), &pool).await?;

        // some sanity checks that the correct metadata record is returned
        assert_eq!(&inserted.build.derivation, &metadata.0.build.derivation);
        assert_eq!(&inserted.git_repo, &metadata.0.git_repo);
        assert_eq!(&inserted.git_commit, &metadata.0.git_commit);
        assert_eq!(&inserted.build_command, &metadata.0.build_command);

        // check that the build attempt was assigned correctly
        assert_eq!(inserted.build.build_attempt.get(), 2u32);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn insert_event(pool: SqlitePool) -> anyhow::Result<()> {
        let event = DrvBuildEvent::for_insert(
            DrvBuildId {
                derivation: DrvId::dummy(),
                build_attempt: NonZeroU32::new(1).unwrap(),
            },
            DrvBuildState::Completed(DrvBuildResult::Success),
        );

        let inserted = new_drv_build_event(event.clone(), &pool).await?;

        // some sanity checks that the correct event record is returned
        assert_eq!(&inserted.build.derivation, &event.0.build.derivation);
        assert_eq!(&inserted.build.build_attempt, &event.0.build.build_attempt);
        assert_eq!(&inserted.state, &event.0.state);

        // we can not check if the time is correct, but we can at least assert that it differs from
        // whatever dummy value we have chosen in the `for_insert` function
        assert_ne!(&inserted.timestamp, &event.0.timestamp);

        Ok(())
    }
}
