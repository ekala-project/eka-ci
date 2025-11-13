use anyhow::{Error, Result};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// This is generic abstraction over a particular "actor" or service.
/// In general, this includes having a channel which can receive tasks, and
/// also run
pub trait AsyncService<T: std::fmt::Debug + 'static>: Sized
where
    T: Send,
    Self: Send + 'static,
{
    fn get_sender(&self) -> mpsc::Sender<T>;
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<T>>;

    fn run(mut self, cancel_token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut receiver = self.take_receiver().expect("receiver was already taken");

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        self.handle_closure().await;
                        break;
                    },
                    maybe_task = receiver.recv() => {
                        match maybe_task {
                            Some(task) => {
                                if let Err(e) = self.handle_task(task).await {
                                    self.handle_failure(e).await;
                                }
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
