//! Database wrappers for various Git related types (mostly from `gitoxide`).

use std::borrow::Cow;

use bstr::ByteSlice;
use sqlx::encode::IsNull;
use sqlx::sqlite::SqliteArgumentValue;
use sqlx::{Decode, Encode, Sqlite, Type};

/// Wrapper around [`gix_url::Url`] that can be encoded and decoded from a SQlite database.
///
/// The URL is encoded as a string when stored in the database, so passwords will be visible as
/// cleartext values. Therefore, do not store URLs with embedded password information in the
/// database.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitRepo(pub gix_url::Url);

impl<'q> Encode<'q, Sqlite> for GitRepo {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<IsNull, sqlx::error::BoxDynError> {
        // Going through bstring keeps the password intact, directly calling `to_string` would
        // redact the password. This is preferrable because it ensures the roundtrip through the
        // database always works. This URL will come from the GitHub application in prod, so it
        // will not contain a password anyways.
        let url = self.0.to_bstring().to_str()?.to_owned();
        buf.push(SqliteArgumentValue::Text(Cow::Owned(url)));

        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, Sqlite> for GitRepo {
    fn decode(
        value: <Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <&str as Decode<Sqlite>>::decode(value)?;
        let url = gix_url::Url::from_bytes(bstr::BStr::new(value))?;

        Ok(GitRepo(url))
    }
}

impl Type<Sqlite> for GitRepo {
    fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
        <str as Type<Sqlite>>::type_info()
    }
}

/// Wrapper around [`gix_hash::ObjectId`] that can be encoded and decoded from a SQlite database.
///
/// The commit object id is stored as hex digits in the database. Always the maximum hex length
/// (i.e. 40 characters for SHA1) is stored.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitCommit(pub gix_hash::ObjectId);

impl<'q> Encode<'q, Sqlite> for GitCommit {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<IsNull, sqlx::error::BoxDynError> {
        let hex = self.0.to_hex().to_string();
        buf.push(SqliteArgumentValue::Text(Cow::Owned(hex)));

        Ok(IsNull::No)
    }

    fn size_hint(&self) -> usize {
        gix_hash::Kind::longest().len_in_hex()
    }
}

impl<'r> Decode<'r, Sqlite> for GitCommit {
    fn decode(
        value: <Sqlite as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <&str as Decode<Sqlite>>::decode(value)?;
        let commit = gix_hash::ObjectId::from_hex(value.as_bytes())?;

        Ok(GitCommit(commit))
    }
}

impl Type<Sqlite> for GitCommit {
    fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
        <str as Type<Sqlite>>::type_info()
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    #[sqlx::test]
    async fn git_repo_roundtrip(pool: SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
CREATE TABLE repos (id INTEGER PRIMARY KEY, repo TEXT NOT NULL)
            "#,
        )
        .execute(&pool)
        .await?;

        let initial = GitRepo(gix_url::parse(
            "https://github.com/ekala-project/eka-ci".into(),
        )?);

        let id = sqlx::query(
            r#"
INSERT INTO repos (repo)
VALUES (?)
            "#,
        )
        .bind(&initial)
        .execute(&pool)
        .await?
        .last_insert_rowid();

        let (_, roundtrip): (i64, GitRepo) = sqlx::query_as(
            r#"
SELECT id, repo FROM repos
WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;

        assert_eq!(initial.0, roundtrip.0);

        Ok(())
    }

    #[sqlx::test]
    async fn git_commit_roundtrip(pool: SqlitePool) -> anyhow::Result<()> {
        // `commit` is a sql keyword so we use `git_commit` instead.
        sqlx::query(
            r#"
CREATE TABLE commits (id INTEGER PRIMARY KEY, git_commit TEXT NOT NULL)
            "#,
        )
        .execute(&pool)
        .await?;

        let initial = GitCommit(gix_hash::ObjectId::from_hex(
            b"1f5cfe6827dc7956af7da54755717202d17667a0",
        )?);

        let id = sqlx::query(
            r#"
INSERT INTO commits (git_commit)
VALUES (?)
            "#,
        )
        .bind(&initial)
        .execute(&pool)
        .await?
        .last_insert_rowid();

        let (_, roundtrip): (i64, GitCommit) = sqlx::query_as(
            r#"
SELECT id, git_commit FROM commits
WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;

        assert_eq!(initial.0, roundtrip.0);

        Ok(())
    }
}
