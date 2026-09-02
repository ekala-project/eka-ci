use anyhow::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::task_journal::{JournalId, TaskJournal};

/// This is generic abstraction over a particular "actor" or service.
/// In general, this includes having a channel which can receive tasks, and
/// also run
pub trait AsyncService<T: std::fmt::Debug + 'static>: Sized
where
    T: Send + Sync + Serialize + for<'de> Deserialize<'de>,
    Self: Send + 'static,
{
    fn get_sender(&self) -> mpsc::Sender<T>;
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<T>>;

    /// Return a reference to the service's task journal if durable
    /// message delivery is enabled. The default implementation returns
    /// `None`, which preserves the existing fire-and-forget behaviour
    /// for services that do not opt in.
    fn task_journal(&self) -> Option<&TaskJournal<T>> {
        None
    }

    fn run(mut self, cancel_token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut receiver = self.take_receiver().expect("receiver was already taken");

            // Replay any un-acknowledged tasks that survived a
            // previous crash. They are re-dispatched before we start
            // consuming new messages so ordering is best-effort.
            self.replay_journal().await;

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        self.handle_closure().await;
                        break;
                    },
                    maybe_task = receiver.recv() => {
                        match maybe_task {
                            Some(task) => {
                                self.dispatch_with_journal(task).await;
                            }
                            None => {
                                warn!("Receiver was closed");
                                break;
                            }
                        }
                    },
                }
            }
        })
    }

    /// Replay un-acknowledged journal entries from a previous run.
    fn replay_journal(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send {
        async {
            let recovered = match self.task_journal() {
                Some(journal) => match journal.recover().await {
                    Ok(entries) => entries,
                    Err(e) => {
                        warn!("failed to recover journal entries: {:?}", e);
                        return;
                    },
                },
                None => return,
            };

            for (jid, task) in recovered {
                info!("replaying recovered task: {:?}", &task);
                if let Err(e) = self.handle_task(task).await {
                    self.handle_failure(e).await;
                }
                ack_journal(self.task_journal(), jid).await;
            }
        }
    }

    /// Persist → handle → acknowledge lifecycle for a single task.
    fn dispatch_with_journal(
        &mut self,
        task: T,
    ) -> impl std::future::Future<Output = ()> + std::marker::Send {
        async {
            let jid = persist_journal(self.task_journal(), &task).await;

            if let Err(e) = self.handle_task(task).await {
                self.handle_failure(e).await;
                // Leave the journal entry in place so the task
                // is replayed on the next startup.
            } else if let Some(id) = jid {
                ack_journal(self.task_journal(), id).await;
            }
        }
    }

    fn handle_task(
        &self,
        task: T,
    ) -> impl std::future::Future<Output = Result<()>> + std::marker::Send;

    fn handle_failure(
        &mut self,
        error: Error,
    ) -> impl std::future::Future<Output = ()> + std::marker::Send;

    fn handle_closure(&mut self) -> impl std::future::Future<Output = ()> + std::marker::Send;
}

/// Persist a task to the journal. Returns `None` when no journal is
/// configured or on serialization/write failure.
async fn persist_journal<T>(journal: Option<&TaskJournal<T>>, task: &T) -> Option<JournalId>
where
    T: std::fmt::Debug + Serialize + for<'de> Deserialize<'de>,
{
    let journal = journal?;
    match journal.persist(task).await {
        Ok(id) => Some(id),
        Err(e) => {
            warn!(
                "failed to journal task {:?}: {:?}; processing without durability",
                task, e
            );
            None
        },
    }
}

/// Acknowledge a journal entry. Logs on failure but never panics.
async fn ack_journal<T>(journal: Option<&TaskJournal<T>>, id: JournalId)
where
    T: std::fmt::Debug + Serialize + for<'de> Deserialize<'de>,
{
    if let Some(journal) = journal {
        if let Err(e) = journal.acknowledge(id).await {
            warn!("failed to ack journal entry: {:?}", e);
        }
    }
}
