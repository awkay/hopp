//! Tauri's handle on the core process connection.
//!
//! All traffic to core goes through [`CoreClient`]: `send` and `start_request` only enqueue
//! on the single ordered outbound queue of the current [`socket_lib::client::Client`], so they
//! never block and nothing can overtake anything else. Waiting for a response happens in
//! [`PendingResponse::wait`], which is async and holds no locks. When core is restarted the
//! connection is swapped with [`CoreClient::install`]; the old one is shut down, which fails
//! its pending requests.

use socket_lib::client::{Client, RequestError};
use socket_lib::{Message, RequestId};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Default time to wait for a response from core.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("core process is not connected")]
    NotConnected,
    #[error("core connection closed")]
    Disconnected,
    #[error("core did not respond in time")]
    Timeout,
    #[error("unexpected response from core")]
    UnexpectedResponse,
}

impl From<RequestError> for CoreError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::Disconnected => CoreError::Disconnected,
        }
    }
}

#[derive(Default)]
pub struct CoreClient {
    // The lock is only held to clone or swap the Arc, never across I/O or waits.
    current: RwLock<Option<Arc<Client>>>,
}

impl CoreClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self) -> Result<Arc<Client>, CoreError> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(CoreError::NotConnected)
    }

    /// Makes `client` the connection used from now on and shuts the previous one down.
    pub fn install(&self, client: Arc<Client>) {
        // The write guard is a temporary dropped at the end of this statement, before the
        // old connection is shut down.
        let previous = self
            .current
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .replace(client);
        if let Some(previous) = previous {
            previous.shutdown();
        }
    }

    /// Shuts the current connection down (pending requests fail) without replacing it.
    pub fn shutdown(&self) {
        // Take it out first so the lock is released before shutting down.
        let client = self
            .current
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(client) = client {
            client.shutdown();
        }
    }

    /// Enqueues an event for core. Never blocks; errors are logged and returned.
    pub fn send(&self, message: Message) -> Result<(), CoreError> {
        let result = self
            .client()
            .and_then(|client| client.send(message).map_err(CoreError::from));
        if let Err(e) = &result {
            log::error!("CoreClient::send: {e}");
        }
        result
    }

    /// Sends `message` and blocks (up to 1 s) until it is written. Only for exit paths,
    /// where the process may end before the writer thread gets to it.
    pub fn send_before_exit(&self, message: Message) {
        if let Ok(client) = self.client() {
            if client.send(message).is_ok() && !client.flush(Duration::from_secs(1)) {
                log::warn!("CoreClient::send_before_exit: message may not have been written");
            }
        }
    }

    /// Enqueues a request. Never blocks: the caller may hold a lock to keep the enqueue
    /// ordered with other state changes, then drop it and `wait` for the response.
    pub fn start_request(&self, message: Message) -> Result<PendingResponse, CoreError> {
        let client = self.client()?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = client.request(
            message,
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        )?;
        Ok(PendingResponse { client, id, rx })
    }

    /// Enqueues a request and waits for its response.
    pub async fn request(&self, message: Message, timeout: Duration) -> Result<Message, CoreError> {
        self.start_request(message)?.wait(timeout).await
    }
}

pub struct PendingResponse {
    client: Arc<Client>,
    id: RequestId,
    rx: tokio::sync::oneshot::Receiver<Result<Message, RequestError>>,
}

impl PendingResponse {
    /// Waits up to `timeout`. On timeout the request is cancelled, so a late response is
    /// discarded instead of being delivered to a later request.
    pub async fn wait(self, timeout: Duration) -> Result<Message, CoreError> {
        match tokio::time::timeout(timeout, self.rx).await {
            Ok(Ok(result)) => result.map_err(CoreError::from),
            Ok(Err(_)) => Err(CoreError::Disconnected),
            Err(_) => {
                self.client.cancel(self.id);
                Err(CoreError::Timeout)
            }
        }
    }
}
