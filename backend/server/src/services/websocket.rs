pub mod events;
pub mod subscription;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use self::events::{ClientMessage, ServerEvent};
use self::subscription::SubscriptionManager;

/// WebSocket service for broadcasting build events
#[derive(Clone)]
pub struct WebSocketService {
    /// Broadcast channel for server events
    event_tx: broadcast::Sender<ServerEvent>,
}

impl WebSocketService {
    /// Create a new WebSocket service
    pub fn new() -> Self {
        // Create broadcast channel with buffer of 100 events
        let (event_tx, _) = broadcast::channel(100);

        Self { event_tx }
    }

    /// Get a sender for broadcasting events
    pub fn event_sender(&self) -> broadcast::Sender<ServerEvent> {
        self.event_tx.clone()
    }

    /// Handle a WebSocket connection
    pub async fn handle_connection(&self, socket: WebSocket) {
        let (mut sender, mut receiver) = socket.split();
        let mut subscription_manager = SubscriptionManager::new();
        let mut event_rx = self.event_tx.subscribe();

        loop {
            tokio::select! {
                // Handle incoming client messages
                msg = receiver.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Err(e) = self.handle_client_message(
                                &text,
                                &mut subscription_manager,
                            ) {
                                error!("Error handling client message: {}", e);
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            debug!("WebSocket connection closed by client");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            debug!("WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }

                // Handle broadcast events
                event = event_rx.recv() => {
                    match event {
                        Ok(server_event) => {
                            // Only send if client is subscribed to this event
                            if subscription_manager.matches(&server_event) {
                                let json = match serde_json::to_string(&server_event) {
                                    Ok(j) => j,
                                    Err(e) => {
                                        error!("Failed to serialize event: {}", e);
                                        continue;
                                    }
                                };

                                if let Err(e) = sender.send(Message::Text(json.into())).await {
                                    error!("Failed to send message to client: {}", e);
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("WebSocket client lagged behind by {} messages", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            debug!("Broadcast channel closed");
                            break;
                        }
                    }
                }
            }
        }

        info!("WebSocket connection closed");
    }

    /// Handle a client message (subscribe/unsubscribe)
    fn handle_client_message(
        &self,
        text: &str,
        subscription_manager: &mut SubscriptionManager,
    ) -> anyhow::Result<()> {
        let client_msg: ClientMessage = serde_json::from_str(text)?;

        match client_msg {
            ClientMessage::Subscribe(sub) => {
                debug!("Client subscribed to {:?} {}", sub.resource, sub.id);
                subscription_manager.subscribe(sub.resource, sub.id);
            },
            ClientMessage::Unsubscribe(unsub) => {
                debug!("Client unsubscribed from {:?} {}", unsub.resource, unsub.id);
                subscription_manager.unsubscribe(unsub.resource, unsub.id);
            },
        }

        Ok(())
    }
}

impl Default for WebSocketService {
    fn default() -> Self {
        Self::new()
    }
}
