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
            let receiver = self.take_receiver().expect("receiver was already taken");
            self.serve(receiver, cancel_token).await;
        })
    }

    /// Replay the journal, then process tasks until `cancel_token` fires
    /// or the channel closes.
    ///
    /// Cancellation is observed while a task is in flight, not just
    /// between tasks: a long-running `handle_task` (e.g. an eval that
    /// traverses tens of thousands of drvs) is dropped as soon as the
    /// token fires. This is safe because an interrupted task is never
    /// acknowledged, so journaled services replay it on the next start.
    fn serve(
        &mut self,
        mut receiver: mpsc::Receiver<T>,
        cancel_token: CancellationToken,
    ) -> impl std::future::Future<Output = ()> + std::marker::Send {
        async move {
            // Replay any un-acknowledged tasks that survived a
            // previous crash. They are re-dispatched before we start
            // consuming new messages so ordering is best-effort.
            if cancel_token
                .run_until_cancelled(self.replay_journal())
                .await
                .is_none()
            {
                warn!("Shutdown interrupted journal replay; remaining tasks replay on next start");
                self.handle_closure().await;
                return;
            }

            while let Some(maybe_task) = cancel_token.run_until_cancelled(receiver.recv()).await {
                let Some(task) = maybe_task else {
                    warn!("Receiver was closed");
                    return;
                };
                let dispatch = self.dispatch_with_journal(task);
                if cancel_token.run_until_cancelled(dispatch).await.is_none() {
                    warn!("Shutdown interrupted an in-flight task; it replays on next start");
                    break;
                }
            }
            self.handle_closure().await;
        }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    /// A service whose tasks never complete, standing in for a long
    /// eval or a hung subprocess.
    struct StuckService {
        sender: mpsc::Sender<String>,
        receiver: Option<mpsc::Receiver<String>>,
        started: Arc<tokio::sync::Notify>,
        closed: Arc<AtomicBool>,
    }

    impl AsyncService<String> for StuckService {
        fn get_sender(&self) -> mpsc::Sender<String> {
            self.sender.clone()
        }

        fn take_receiver(&mut self) -> Option<mpsc::Receiver<String>> {
            self.receiver.take()
        }

        async fn handle_task(&self, _task: String) -> Result<()> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn handle_failure(&mut self, _error: Error) {}

        async fn handle_closure(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_in_flight_task() {
        let (sender, receiver) = mpsc::channel(1);
        let started = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(AtomicBool::new(false));
        let service = StuckService {
            sender: sender.clone(),
            receiver: Some(receiver),
            started: started.clone(),
            closed: closed.clone(),
        };
        let token = CancellationToken::new();
        let handle = service.run(token.clone());

        sender.send("task".to_string()).await.expect("send task");
        started.notified().await;
        token.cancel();

        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("service did not exit after cancellation")
            .expect("service task panicked");
        assert!(closed.load(Ordering::SeqCst), "handle_closure not called");
    }
}
