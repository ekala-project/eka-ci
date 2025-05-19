use super::build::DrvBuildId;
use super::drv::DrvId;
use super::ForInsert;
use futures::stream::BoxStream;
use sqlx::SqlitePool;
use sqlx::{FromRow, Pool, Sqlite};

/// Emitted whenever a derivation build's state changes.
#[derive(Clone, Debug, FromRow)]
pub struct DrvBuildEvent {
    /// The derivation build this event is associated with.
    #[sqlx(flatten)]
    pub build: DrvBuildId,

    /// The build state this event propagates.
    pub state: DrvBuildState,

    /// The timestamp when this event happened.
    ///
    /// This timestamp only has second accuracy, which makes it unsuitable for sorting of build
    /// events. If for example the build queue is empty, it is not unlikely that a build is
    /// scheduled ([`DrvBuildState::Pending`]) and started ([`DrvBuildState::Building`]) within the
    /// same second.
    ///
    /// Instead, use the table's ROWID to sort the events during select.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl DrvBuildEvent {
    pub fn for_insert(build: DrvBuildId, state: DrvBuildState) -> ForInsert<Self> {
        ForInsert(Self {
            build,
            state,
            timestamp: chrono::DateTime::<chrono::Utc>::MAX_UTC,
        })
    }
}

/// Describes the possible states a derivation build can be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildState {
    /// Derivation is waiting to be scheduled for building.
    ///
    /// The evaluator has determined that this derivation needs be built and has sent it to the
    /// scheduler. The derivation stays in this state until the scheduler decides that it is ready
    /// to be built, which mostly means until all its dependencies have been built.
    Queued,
    /// Derivation is waiting to be built.
    ///
    /// The scheduler has determined that this derivation is ready to be built. The derivation
    /// stays in this state until a builder picks it up to perform the actual build step.
    Buildable,
    /// Derivation is building.
    ///
    /// A builder has picked this derivation up and is now realizing the derivation. The derivation
    /// build stays in this state until the build completes or is interrupted.
    Building,
    /// Derivation has been built, either successfully or not.
    ///
    /// This is a terminal state, a derivation build will never leave this state. Depending on the
    /// outcome of the built, the state of other derivation builds may be changed. If the build
    /// completed successfully, all direct dependants will be marked as buildable. If the build
    /// failed, all transitive dependants will be marked as transitive failure.
    Completed(DrvBuildResult),
    /// Build was interrupted before it could complete.
    ///
    /// For some interruption kinds, the build will be retried automatically. In those cases, the
    /// build will be immediately marked as buildable again. Dependants are not affected.
    ///
    /// For most interruption kinds however, an automatic retry makes no sense. A new attempt at
    /// building the derivation may be queued manually or when the job configuration changed. All
    /// transitive dependants of this derivation will be marked as blocked, until the next build
    /// attempt. This derivation build will never leave this state in that case.
    Interrupted(DrvBuildInterruptionKind),
    /// At least one transitive dependency of this build has failed.
    ///
    /// This is a terminal state, a derivation build will never leave this state.
    TransitiveFailure,
    /// At least one transitive dependency of this build has been interrupted.
    ///
    /// A failing build of another transitive dependency has a higher precedence than this. The
    /// transitive failure state therefore takes priority over this state and overwrite it.
    ///
    /// Otherwise, the derivation build stays in this state until a later build attempt of the
    /// dependency completes. Every time a build attempt completes, the scheduler checks if a
    /// previous build attempt has been interrupted, and if so, unblocks all transitive dependants
    /// again. Once a derivation build is unblocked, it will be queued again.
    Blocked,
}

/// See if there is a build state associated with a Drv
///   None -> No build has been queued
///   Some(state) -> State of latest build event
pub async fn latest_build_state(
    drv: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<DrvBuildState>> {
    // Order by rowid, assuming that the latest will have the highest count
    let maybe_state = sqlx::query_scalar(
        r#"
SELECT state FROM DrvBuildEvent
WHERE derivation = ?1
ORDER BY rowid ASC
LIMIT 1
        "#,
    )
    .bind(drv)
    .fetch_optional(pool)
    .await?;
    let maybe_state =
        maybe_state.map(|x: i8| DrvBuildState::try_from_i8(x).expect("Invalid build state value"));

    Ok(maybe_state)
}

/// The result of building a derivation.
///
/// In essence, this enum captures whether the status code returned by the build command was `0`
/// or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildResult {
    /// The derivation built successfully.
    Success,
    /// The derivation failed to build.
    Failure,
}

impl DrvBuildResult {
    /// Handy helper that allows processing the build result in a more functional style using
    /// [map][Result::map], [map_err][Result::map_err], [map_or_else][Result::map_or_else] and
    /// the like.
    pub fn as_result(&self) -> Result<(), ()> {
        match self {
            DrvBuildResult::Success => Ok(()),
            DrvBuildResult::Failure => Err(()),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure)
    }
}

/// Possible causes for why the derivation build was interrupted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrvBuildInterruptionKind {
    /// Build process ran out of memory and was killed by the system.
    OutOfMemory,
    /// Build process timed out and was killed by the build scheduler.
    Timeout,
    /// Scheduler process performed a graceful shutdown and cancelled the derivation build in the
    /// process.
    Cancelled,
    /// Build process died for unknown reasons, most likely a fault in the build command.
    ProcessDeath,
    /// Scheduler process died. The scheduler can infer that this happend by checking for
    /// derivation builds which do not have the status [`DrvBuildState::Completed`] whilst
    /// starting.
    SchedulerDeath,
}

mod state {
    use sqlx::{Decode, Encode, Sqlite, Type};

    use super::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    #[derive(sqlx::Type)]
    #[repr(i8)]
    enum DrvBuildStateRepr {
        Queued = 0,
        Buildable = 1,
        Building = 7,
        CompletedSuccess = 42,
        CompletedFailure = -1,
        TransitiveFailure = -2,
        InterruptedOutOfMemory = -104,
        InterruptedTimeout = -120,
        InterruptedCancelled = -86,
        InterruptedProcessDeath = -66,
        InterruptedSchedulerDeath = -13,
        Blocked = 100,
    }

    impl DrvBuildStateRepr {
        pub fn try_from(value: i8) -> anyhow::Result<DrvBuildStateRepr> {
            match value {
                x if x == Self::Queued as i8 => Ok(Self::Queued),
                x if x == Self::Buildable as i8 => Ok(Self::Buildable),
                x if x == Self::Building as i8 => Ok(Self::Building),
                x if x == Self::CompletedSuccess as i8 => Ok(Self::CompletedSuccess),
                x if x == Self::CompletedFailure as i8 => Ok(Self::CompletedFailure),
                x if x == Self::TransitiveFailure as i8 => Ok(Self::TransitiveFailure),
                x if x == Self::InterruptedOutOfMemory as i8 => Ok(Self::InterruptedOutOfMemory),
                x if x == Self::InterruptedTimeout as i8 => Ok(Self::InterruptedTimeout),
                x if x == Self::InterruptedCancelled as i8 => Ok(Self::InterruptedCancelled),
                x if x == Self::InterruptedProcessDeath as i8 => Ok(Self::InterruptedProcessDeath),
                x if x == Self::InterruptedSchedulerDeath as i8 => {
                    Ok(Self::InterruptedSchedulerDeath)
                }
                x if x == Self::Blocked as i8 => Ok(Self::Blocked),
                _ => Err(anyhow::Error::msg("Invalid build state repr vlue")),
            }
        }
    }

    impl DrvBuildState {
        pub fn try_from_i8(value: i8) -> anyhow::Result<DrvBuildState> {
            let repr: DrvBuildStateRepr = DrvBuildStateRepr::try_from(value)?;
            Ok(DrvBuildState::from(repr))
        }
    }

    impl From<&DrvBuildState> for DrvBuildStateRepr {
        fn from(value: &DrvBuildState) -> Self {
            match value {
                DrvBuildState::Queued => Self::Queued,
                DrvBuildState::Buildable => Self::Buildable,
                DrvBuildState::Building => Self::Building,
                DrvBuildState::Completed(DrvBuildResult::Success) => Self::CompletedSuccess,
                DrvBuildState::Completed(DrvBuildResult::Failure) => Self::CompletedFailure,
                DrvBuildState::TransitiveFailure => Self::TransitiveFailure,
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => {
                    Self::InterruptedOutOfMemory
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => {
                    Self::InterruptedTimeout
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
                    Self::InterruptedCancelled
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => {
                    Self::InterruptedProcessDeath
                }
                DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => {
                    Self::InterruptedSchedulerDeath
                }
                DrvBuildState::Blocked => Self::Blocked,
            }
        }
    }

    impl From<DrvBuildStateRepr> for DrvBuildState {
        fn from(value: DrvBuildStateRepr) -> Self {
            match value {
                DrvBuildStateRepr::Queued => Self::Queued,
                DrvBuildStateRepr::Buildable => Self::Buildable,
                DrvBuildStateRepr::Building => Self::Building,
                DrvBuildStateRepr::CompletedSuccess => Self::Completed(DrvBuildResult::Success),
                DrvBuildStateRepr::CompletedFailure => Self::Completed(DrvBuildResult::Failure),
                DrvBuildStateRepr::TransitiveFailure => Self::TransitiveFailure,
                DrvBuildStateRepr::InterruptedOutOfMemory => {
                    Self::Interrupted(DrvBuildInterruptionKind::OutOfMemory)
                }
                DrvBuildStateRepr::InterruptedTimeout => {
                    Self::Interrupted(DrvBuildInterruptionKind::Timeout)
                }
                DrvBuildStateRepr::InterruptedCancelled => {
                    Self::Interrupted(DrvBuildInterruptionKind::Cancelled)
                }
                DrvBuildStateRepr::InterruptedProcessDeath => {
                    Self::Interrupted(DrvBuildInterruptionKind::ProcessDeath)
                }
                DrvBuildStateRepr::InterruptedSchedulerDeath => {
                    Self::Interrupted(DrvBuildInterruptionKind::SchedulerDeath)
                }
                DrvBuildStateRepr::Blocked => Self::Blocked,
            }
        }
    }

    impl<'q> Encode<'q, Sqlite> for DrvBuildState {
        fn encode_by_ref(
            &self,
            buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
        ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
            <DrvBuildStateRepr as Encode<'q, Sqlite>>::encode_by_ref(&self.into(), buf)
        }

        fn size_hint(&self) -> usize {
            <DrvBuildStateRepr as Encode<'q, Sqlite>>::size_hint(&self.into())
        }
    }

    impl<'r> Decode<'r, Sqlite> for DrvBuildState {
        fn decode(
            value: <Sqlite as sqlx::Database>::ValueRef<'r>,
        ) -> Result<Self, sqlx::error::BoxDynError> {
            Ok(<DrvBuildStateRepr as Decode<Sqlite>>::decode(value)?.into())
        }
    }

    impl Type<Sqlite> for DrvBuildState {
        fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
            <DrvBuildStateRepr as Type<Sqlite>>::type_info()
        }

        fn compatible(ty: &<Sqlite as sqlx::Database>::TypeInfo) -> bool {
            <DrvBuildStateRepr as Type<Sqlite>>::compatible(ty)
        }
    }
}

/// Given a DrvId, what is theh build_event status of all dependencies
pub async fn get_latest_build_events_for_deps(
    derivation: &DrvId,
    pool: &SqlitePool,
) -> anyhow::Result<Vec<DrvBuildEvent>> {
    let events = sqlx::query_as(
        r#"
SELECT MAX(rowid), derivation, build_attempt, state, timestamp
FROM DrvBuildEvent
JOIN DrvRefs ON DrvBuildEvent.derivation = DrvRefs.reference
WHERE referrer = ?
        "#,
    )
    .bind(derivation)
    .fetch_all(pool)
    .await?;

    Ok(events)
}

/// Determine if a Drv can be built
/// One issue with this logic is a missing dependency drv in the db would be treated
/// as "successful"
pub async fn is_drv_buildable(
    derivation: &DrvId,
    pool: &SqlitePool,
) -> anyhow::Result<bool> {
    let deps = get_latest_build_events_for_deps(derivation, pool).await?;

    let result = deps
        .into_iter()
        .all(|x| x.state == DrvBuildState::Completed(DrvBuildResult::Success));

    Ok(result)
}


pub async fn get_latest_build_event(
    derivation: &DrvId,
    pool: &SqlitePool,
) -> anyhow::Result<Option<DrvBuildEvent>> {
    let event = sqlx::query_as(
        r#"
SELECT MAX(rowid), derivation, build_attempt, state, timestamp
FROM DrvBuildEvent
WHERE derivation = ?
        "#,
    )
    .bind(derivation)
    .fetch_optional(pool)
    .await?;

    Ok(event)
}

pub fn get_derivations_in_state<'a>(
    state: DrvBuildState,
    pool: &'a SqlitePool,
) -> BoxStream<'a, sqlx::Result<DrvBuildEvent>> {
    sqlx::query_as(
        r#"
SELECT MAX(rowid), derivation, build_attempt, state, timestamp
FROM DrvBuildEvent
GROUP BY derivation
HAVING state = ?
        "#,
    )
    .bind(state)
    .fetch(pool)
}

#[cfg(test)]

mod tests {
    use super::get_derivations_in_state;
    use super::{get_latest_build_event, DrvBuildState};
    use crate::db::model::drv::DrvId;
    use anyhow::bail;
    use futures::StreamExt;
    use sqlx::SqlitePool;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn select_latest_state(pool: SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO DrvBuildEvent
    (derivation, build_attempt, state)
VALUES
(?1, 1, ?2),
(?1, 1, ?3),
(?1, 2, ?4),
(?1, 2, ?5)
            "#,
        )
        .bind(DrvId::dummy())
        .bind(DrvBuildState::Queued)
        .bind(DrvBuildState::Buildable)
        .bind(DrvBuildState::Building)
        .bind(DrvBuildState::Blocked)
        .execute(&pool)
        .await?;

        let Some(result) = get_latest_build_event(&DrvId::dummy(), &pool).await? else {
            bail!("Expected query to find a result")
        };

        assert_eq!(result.state, DrvBuildState::Blocked);
        Ok(())
    }

    #[ignore = "Need to use valid derivations in sample data, otherwise stuff fails"]
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn select_multiple_derivations_in_state(pool: SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO DrvBuildEvent
    (derivation, build_attempt, state)
VALUES
('drv1', 1, ?1),
('drv1', 1, ?2),
('drv1', 1, ?3),
('drv2', 1, ?3),
('drv2', 1, ?4),
('drv3', 1, ?3)
            "#,
        )
        .bind(DrvBuildState::Queued)
        .bind(DrvBuildState::Buildable)
        .bind(DrvBuildState::Building)
        .bind(DrvBuildState::Blocked)
        .execute(&pool)
        .await?;

        let result: Vec<_> = get_derivations_in_state(DrvBuildState::Building, &pool)
            .collect()
            .await;

        dbg!(result);
        assert!(false);
        Ok(())
    }
}
