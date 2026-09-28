//! Request/response client for the Tauri side of the IPC socket.
//!
//! - One ordered outbound queue: every message (events and requests) goes through
//!   [`Client::send`] / [`Client::request`], which only enqueue; a writer thread does the
//!   socket I/O. Nothing can overtake anything else.
//! - Requests carry a [`RequestId`]; core echoes it on the response. Requests are pipelined:
//!   several can be in flight at once.
//! - One dispatcher thread handles incoming frames in wire order: events go to
//!   [`IncomingHandler::on_event`]; responses are shown to [`IncomingHandler::on_response`]
//!   first (so state changes tied to a response are applied in order with events) and then
//!   complete the waiting request. Responses nobody waits for any more are discarded.
//! - The caller owns timeouts: it calls [`Client::cancel`] when it gives up, and a late
//!   response is then dropped.

use crate::{EventSocket, Frame, Message, RequestId, SocketSender};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// The connection closed (or the client was shut down) before a response arrived.
    Disconnected,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Disconnected => write!(f, "core connection closed"),
        }
    }
}

impl std::error::Error for RequestError {}

/// Called exactly once with the response, or with an error if the connection closed.
/// Not called if the request was cancelled.
pub type Completion = Box<dyn FnOnce(Result<Message, RequestError>) + Send>;

/// Receives incoming traffic on the dispatcher thread, in wire order.
pub trait IncomingHandler: Send + 'static {
    fn on_event(&mut self, message: Message);
    /// Sees every response (matched or not) before its request is completed.
    fn on_response(&mut self, _message: &Message) {}
    /// The connection closed; no more calls follow.
    fn on_disconnect(&mut self) {}
}

/// Pending-request bookkeeping, separate from I/O so it can be tested directly.
pub(crate) struct PendingRequests {
    next_id: RequestId,
    entries: HashMap<RequestId, Completion>,
    closed: bool,
}

impl PendingRequests {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            entries: HashMap::new(),
            closed: false,
        }
    }

    /// Registers a completion and returns its id, or gives the completion back if closed.
    pub(crate) fn register(&mut self, completion: Completion) -> Result<RequestId, Completion> {
        if self.closed {
            return Err(completion);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(id, completion);
        Ok(id)
    }

    /// Removes and returns the completion waiting for `id`; `None` for unknown/stale ids.
    pub(crate) fn take(&mut self, id: RequestId) -> Option<Completion> {
        self.entries.remove(&id)
    }

    /// Marks the set closed and returns all outstanding completions.
    pub(crate) fn close(&mut self) -> Vec<Completion> {
        self.closed = true;
        self.entries
            .drain()
            .map(|(_, completion)| completion)
            .collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

enum Outbound {
    Frame(Frame),
    /// Signals once everything enqueued before it has been written.
    Flush(mpsc::Sender<()>),
}

pub struct Client {
    outbound: mpsc::Sender<Outbound>,
    pending: Arc<Mutex<PendingRequests>>,
    socket: SocketSender,
    /// Set by `shutdown`; the dispatcher then drops frames still buffered from this
    /// connection instead of delivering them.
    closed: Arc<AtomicBool>,
}

impl Client {
    /// Starts the writer and dispatcher threads for an established connection.
    pub fn start(
        sender: SocketSender,
        mut event_socket: EventSocket,
        mut handler: impl IncomingHandler,
    ) -> Arc<Client> {
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let (outbound_tx, outbound_rx) = mpsc::channel::<Outbound>();

        let writer_socket = sender.clone();
        std::thread::Builder::new()
            .name("core-ipc-writer".into())
            .spawn(move || {
                for item in outbound_rx {
                    match item {
                        Outbound::Frame(frame) => {
                            if let Err(e) = writer_socket.send_frame(&frame) {
                                log::error!(
                                    "core-ipc-writer: write failed, closing connection: {e:?}"
                                );
                                writer_socket.shutdown();
                                break;
                            }
                        }
                        Outbound::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
                log::info!("core-ipc-writer: exiting");
            })
            .expect("failed to spawn core-ipc-writer");

        let dispatcher_pending = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let dispatcher_closed = closed.clone();
        std::thread::Builder::new()
            .name("core-ipc-dispatcher".into())
            .spawn(move || {
                let incoming = event_socket.take_incoming();
                for frame in incoming.iter() {
                    if dispatcher_closed.load(Ordering::SeqCst) {
                        log::info!("core-ipc-dispatcher: client shut down, dropping buffered frames");
                        break;
                    }
                    match frame.request_id {
                        None => handler.on_event(frame.message),
                        Some(id) => {
                            handler.on_response(&frame.message);
                            let completion = dispatcher_pending
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take(id);
                            match completion {
                                Some(completion) => completion(Ok(frame.message)),
                                None => log::warn!(
                                    "core-ipc-dispatcher: discarding response {id} nobody waits for: {:?}",
                                    frame.message
                                ),
                            }
                        }
                    }
                }
                let outstanding = dispatcher_pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .close();
                for completion in outstanding {
                    completion(Err(RequestError::Disconnected));
                }
                handler.on_disconnect();
                drop(event_socket);
                log::info!("core-ipc-dispatcher: exiting");
            })
            .expect("failed to spawn core-ipc-dispatcher");

        Arc::new(Client {
            outbound: outbound_tx,
            pending,
            socket: sender,
            closed,
        })
    }

    /// Enqueues an event. Never blocks.
    pub fn send(&self, message: Message) -> Result<(), RequestError> {
        self.outbound
            .send(Outbound::Frame(Frame::event(message)))
            .map_err(|_| RequestError::Disconnected)
    }

    /// Enqueues a request. Never blocks. `completion` runs on the dispatcher thread
    /// (or right here if the client is already closed, in which case `Err` is returned too).
    pub fn request(
        &self,
        message: Message,
        completion: Completion,
    ) -> Result<RequestId, RequestError> {
        let registered = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .register(completion);
        let id = match registered {
            Ok(id) => id,
            Err(completion) => {
                completion(Err(RequestError::Disconnected));
                return Err(RequestError::Disconnected);
            }
        };
        if self
            .outbound
            .send(Outbound::Frame(Frame::with_id(Some(id), message)))
            .is_err()
        {
            let completion = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(id);
            if let Some(completion) = completion {
                completion(Err(RequestError::Disconnected));
            }
            return Err(RequestError::Disconnected);
        }
        Ok(id)
    }

    /// Blocks until everything enqueued so far has been written, or `timeout` passes.
    /// Only for shutdown paths (e.g. sending CallEnd right before the app exits).
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        let (done_tx, done_rx) = mpsc::channel();
        if self.outbound.send(Outbound::Flush(done_tx)).is_err() {
            return false;
        }
        done_rx.recv_timeout(timeout).is_ok()
    }

    /// Stops waiting for `id`; its response, if it still arrives, is discarded.
    pub fn cancel(&self, id: RequestId) {
        let _ = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(id);
    }

    /// Fails all pending requests now and closes the connection.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let outstanding = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close();
        for completion in outstanding {
            completion(Err(RequestError::Disconnected));
        }
        self.socket.shutdown();
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_pair;
    use crate::CallStartResultMessage;
    use std::time::Duration;

    fn completion_into(tx: mpsc::Sender<Result<Message, RequestError>>) -> Completion {
        Box::new(move |result| {
            let _ = tx.send(result);
        })
    }

    #[test]
    fn pending_ids_are_unique_and_take_is_one_shot() {
        let mut pending = PendingRequests::new();
        let a = pending.register(Box::new(|_| {})).ok().unwrap();
        let b = pending.register(Box::new(|_| {})).ok().unwrap();
        assert_ne!(a, b);
        assert!(pending.take(a).is_some());
        assert!(
            pending.take(a).is_none(),
            "a second response for the same id is stale"
        );
        assert!(pending.take(999).is_none(), "unknown ids are stale");
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn pending_close_returns_outstanding_and_rejects_new() {
        let mut pending = PendingRequests::new();
        pending.register(Box::new(|_| {})).ok().unwrap();
        pending.register(Box::new(|_| {})).ok().unwrap();
        assert_eq!(pending.close().len(), 2);
        assert!(pending.register(Box::new(|_| {})).is_err());
    }

    #[derive(Debug, PartialEq)]
    enum Seen {
        Event(String),
        Response(String),
        Disconnect,
    }

    struct Recorder(mpsc::Sender<Seen>);

    impl IncomingHandler for Recorder {
        fn on_event(&mut self, message: Message) {
            let _ = self.0.send(Seen::Event(format!("{message:?}")));
        }
        fn on_response(&mut self, message: &Message) {
            let _ = self.0.send(Seen::Response(format!("{message:?}")));
        }
        fn on_disconnect(&mut self) {
            let _ = self.0.send(Seen::Disconnect);
        }
    }

    fn start_client() -> (
        Arc<Client>,
        mpsc::Receiver<Seen>,
        SocketSender,
        crate::EventSocket,
    ) {
        let ((server_sender, server_events), (client_sender, client_events)) = test_pair();
        let (seen_tx, seen_rx) = mpsc::channel();
        let client = Client::start(client_sender, client_events, Recorder(seen_tx));
        (client, seen_rx, server_sender, server_events)
    }

    fn recv_frame(events: &crate::EventSocket) -> Frame {
        events
            .incoming
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn pipelined_requests_match_out_of_order_responses() {
        let (client, _seen, server_sender, server_events) = start_client();
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        client
            .request(Message::ListCameras, completion_into(tx_a))
            .unwrap();
        client
            .request(Message::ListAudioDevices, completion_into(tx_b))
            .unwrap();

        let first = recv_frame(&server_events);
        let second = recv_frame(&server_events);
        assert!(matches!(first.message, Message::ListCameras));
        assert!(matches!(second.message, Message::ListAudioDevices));

        // Answer in reverse order.
        server_sender
            .send_with_id(second.request_id, Message::AudioDeviceList(vec![]))
            .unwrap();
        server_sender
            .send_with_id(first.request_id, Message::CameraList(vec![]))
            .unwrap();

        let timeout = Duration::from_secs(5);
        assert!(matches!(
            rx_a.recv_timeout(timeout).unwrap(),
            Ok(Message::CameraList(_))
        ));
        assert!(matches!(
            rx_b.recv_timeout(timeout).unwrap(),
            Ok(Message::AudioDeviceList(_))
        ));
        assert_eq!(client.pending_count(), 0);
    }

    #[test]
    fn late_response_after_cancel_is_discarded_and_next_request_unaffected() {
        let (client, _seen, server_sender, server_events) = start_client();
        let (tx_old, rx_old) = mpsc::channel();
        let old_id = client
            .request(Message::ListCameras, completion_into(tx_old))
            .unwrap();
        let old = recv_frame(&server_events);
        client.cancel(old_id);

        let (tx_new, rx_new) = mpsc::channel();
        client
            .request(Message::ListCameras, completion_into(tx_new))
            .unwrap();
        let new = recv_frame(&server_events);

        let camera = |name: &str| crate::CameraDevice {
            name: name.into(),
            id: name.into(),
            default: false,
        };
        // Late answer to the cancelled request arrives first.
        server_sender
            .send_with_id(old.request_id, Message::CameraList(vec![camera("old")]))
            .unwrap();
        server_sender
            .send_with_id(new.request_id, Message::CameraList(vec![camera("new")]))
            .unwrap();

        match rx_new.recv_timeout(Duration::from_secs(5)).unwrap() {
            Ok(Message::CameraList(list)) => assert_eq!(list[0].name, "new"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(rx_old.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn events_and_responses_reach_the_handler_in_wire_order() {
        let (client, seen, server_sender, server_events) = start_client();
        let (tx, rx) = mpsc::channel();
        client
            .request(
                Message::CallStart(crate::CallStartMessage {
                    call_id: 9,
                    audio_token: String::new(),
                    video_token: String::new(),
                    audio_device_name: String::new(),
                    start_mic_on_call: None,
                    start_camera_on_call: None,
                }),
                completion_into(tx),
            )
            .unwrap();
        let request = recv_frame(&server_events);

        server_sender.send(Message::CallEnded(8)).unwrap();
        server_sender
            .send_with_id(
                request.request_id,
                Message::CallStartResult(CallStartResultMessage {
                    call_id: 9,
                    result: Ok(()),
                }),
            )
            .unwrap();
        server_sender.send(Message::CallEnded(9)).unwrap();

        let timeout = Duration::from_secs(5);
        let order: Vec<Seen> = (0..3)
            .map(|_| seen.recv_timeout(timeout).unwrap())
            .collect();
        assert!(matches!(&order[0], Seen::Event(e) if e.contains("CallEnded(8)")));
        assert!(matches!(&order[1], Seen::Response(r) if r.contains("CallStartResult")));
        assert!(matches!(&order[2], Seen::Event(e) if e.contains("CallEnded(9)")));
        assert!(rx.recv_timeout(timeout).unwrap().is_ok());
    }

    #[test]
    fn shutdown_fails_pending_and_rejects_new_requests() {
        let (client, seen, _server_sender, _server_events) = start_client();
        let (tx, rx) = mpsc::channel();
        client
            .request(Message::ListCameras, completion_into(tx))
            .unwrap();
        client.shutdown();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap_err(),
            RequestError::Disconnected
        );

        let (tx2, rx2) = mpsc::channel();
        assert!(client
            .request(Message::ListCameras, completion_into(tx2))
            .is_err());
        assert!(rx2.recv_timeout(Duration::from_secs(5)).unwrap().is_err());

        // The dispatcher sees end-of-stream and reports it.
        loop {
            match seen.recv_timeout(Duration::from_secs(5)).unwrap() {
                Seen::Disconnect => break,
                _ => continue,
            }
        }
    }

    #[test]
    fn flush_returns_after_queued_frames_are_written() {
        let (client, _seen, _server_sender, server_events) = start_client();
        client.send(Message::CallEnd(None)).unwrap();
        assert!(client.flush(Duration::from_secs(5)));
        assert!(matches!(
            recv_frame(&server_events).message,
            Message::CallEnd(None)
        ));
    }

    #[test]
    fn frames_buffered_before_shutdown_are_not_delivered() {
        let (client, seen, server_sender, _server_events) = start_client();
        client.shutdown();
        // Anything still arriving (or buffered) for a shut-down client is dropped.
        let _ = server_sender.send(Message::MicrophoneAudioLevel(0.5));
        assert_eq!(
            seen.recv_timeout(Duration::from_secs(5)).unwrap(),
            Seen::Disconnect,
            "nothing may be delivered after shutdown"
        );
    }

    #[test]
    fn shutdown_does_not_wait_for_a_blocked_writer() {
        // The peer end is never read, so the writer thread blocks in write_all while
        // holding the writer mutex.
        let (ours, _unread_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (sender, events) = crate::build_pair(ours).unwrap();
        let (seen_tx, _seen_rx) = mpsc::channel();
        let client = Client::start(sender, events, Recorder(seen_tx));
        let big = "x".repeat(1024 * 1024);
        for _ in 0..16 {
            client
                .send(Message::SetAppVeilBundleIds(vec![big.clone()]))
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(300));
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown_client = client.clone();
        std::thread::spawn(move || {
            shutdown_client.shutdown();
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "shutdown blocked behind the writer"
        );
    }

    #[test]
    fn peer_disconnect_fails_pending() {
        let (client, _seen, server_sender, server_events) = start_client();
        let (tx, rx) = mpsc::channel();
        client
            .request(Message::ListCameras, completion_into(tx))
            .unwrap();
        let _ = recv_frame(&server_events);
        server_sender.shutdown();
        drop(server_events);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap_err(),
            RequestError::Disconnected
        );
    }
}
