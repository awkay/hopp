use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use livekit::options::{
    DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoding, VideoEncodingUpdate,
};
use livekit::participant::{ConnectionQuality, LocalParticipant, Participant};
use livekit::prelude::TrackSid;
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource, VideoQuality};
use livekit::webrtc::prelude::{RtcVideoSource, VideoResolution};
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::{DataPacket, Room, RoomEvent, RoomOptions};
use thread_priority::{set_current_thread_priority, ThreadPriority};
use tokio::runtime::Handle as TokioHandle;

use crate::audio::mixer::SharedProcessor;
use crate::bandwidth_mode::{
    default_screen_bitrate, screen_encoding, BandwidthModeRequest, BandwidthNegotiation,
    MAX_FRAMERATE, SCREEN_SHARE_USES_AV1,
};
use crate::livekit::audio::AudioPublisher;
use crate::livekit::participant::ParticipantInfo;
use crate::livekit::video::{process_video_stream, VideoBufferManager};
use crate::screen_share_codec::{
    this_mac_may_share_hevc, EncoderCheck, EncoderCheckStep, ScreenShareCodec,
    ScreenShareCodecChoice, ENCODER_CHECK_IDLE_LIMIT,
};
use crate::window::screensharing_window::RedrawCommand;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};
use winit::event_loop::EventLoopProxy;

use crate::effects::wire::{encode_effect_packet, parse_effect_packet, TOPIC_EFFECT};
use crate::snapshot_sender::SnapshotSender;
use crate::{audio, ParticipantData, UserEvent};
use socket_lib::screen_share_hevc;

// Constants for magic values
const TOPIC_SHARER_LOCATION: &str = "participant_location";
const TOPIC_REMOTE_CONTROL_ENABLED: &str = "remote_control_enabled";
const TOPIC_PARTICIPANT_IN_CONTROL: &str = "participant_in_control";
const TOPIC_TICK_RESPONSE: &str = "tick_response";
const VIDEO_TRACK_NAME: &str = "screen_share";
const TOPIC_DRAW: &str = "draw";
const TOPIC_APP_VEIL: &str = "app_veil";
const TOPIC_BANDWIDTH_MODE: &str = "bandwidth_mode";
const CAMERA_TRACK_NAME: &str = "camera";
/// Dev override of the screen share's codec. See `screen_share_codec_override`.
const SCREEN_SHARE_CODEC_ENV: &str = "HOPP_SCREEN_SHARE_CODEC";
/// How long a watcher that joined without announcing its HEVC support has to announce it before
/// it counts as without (spec 0007). A Hopp client announces right after it connects, which can
/// reach us after its join; older clients and browsers never announce, and B3 allows 2 s from
/// their join to the end of the switch to H.264. One that publishes a track first counts as
/// without at once: a Hopp client announces before it publishes.
const HEVC_ANNOUNCEMENT_GRACE: Duration = Duration::from_millis(750);
/// Upper bound on waiting for the server to confirm our HEVC announcement. The command task
/// waits for it after a setting change or a reconnect, and a server that never answers mustn't
/// hold up the commands behind it. The announcement is sent either way.
const HEVC_ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a new screen track, or one just unmuted, is checked for encoding (spec 0007 B6 for
/// HEVC, `EncoderCheck`).
const SCREEN_SHARE_ENCODER_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// How long a screen track published during a share keeps getting the last captured frame
/// resent, so viewers subscribing to it get a picture on a static screen. See
/// `check_screen_share_encoder`.
const SCREEN_SHARE_TRACK_WARMUP: Duration = Duration::from_secs(3);
/// How many times a screen track republish tries to publish the new track, and how long it
/// waits between tries.
const SCREEN_SHARE_PUBLISH_ATTEMPTS: u32 = 2;
const SCREEN_SHARE_PUBLISH_RETRY_DELAY: Duration = Duration::from_secs(1);
/// How long a viewer keeps the screen-share window open after the shown track was unpublished
/// while live, waiting for the sharer's replacement track (a codec switch, spec 0007).
const SCREEN_SHARE_REPLACEMENT_GRACE: Duration = Duration::from_secs(5);
const CAMERA_MAX_BITRATE: u64 = 1_700_000;
const CAMERA_MAX_FRAMERATE: f64 = 30.0;
/// How long a muted camera or screen track keeps sending keepalive frames after it is
/// published. See `end_video_keepalive`.
const VIDEO_KEEPALIVE_GRACE: Duration = Duration::from_secs(5);
/// Per-attempt LiveKit signal connect timeout (websocket + TLS). The SDK default is 5s,
/// which a 3G link can't meet: every attempt timed out mid-handshake and retried forever.
const SIGNAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound on connecting a room and publishing its initial tracks. Leaves room for
/// the SDK's join retries at `SIGNAL_CONNECT_TIMEOUT` on a slow link.
const ROOM_SETUP_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct NormalizedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppVeilWindow {
    pub frame: NormalizedRect,
    pub visible_fragments: Vec<NormalizedRect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AppVeilSnapshot {
    pub windows: Vec<AppVeilWindow>,
    pub keyboard_input_blocked: bool,
}

pub(crate) fn canonical_participant_identity(identity: &str) -> &str {
    identity
        .strip_suffix(":audio")
        .or_else(|| identity.strip_suffix(":video"))
        .unwrap_or(identity)
}

fn participant_identities_match(left: &str, right: &str) -> bool {
    canonical_participant_identity(left) == canonical_participant_identity(right)
}

fn sanitize_normalized_rect(rect: NormalizedRect) -> Option<NormalizedRect> {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return None;
    }
    let x = rect.x.clamp(0.0, 1.0);
    let y = rect.y.clamp(0.0, 1.0);
    let right = (rect.x + rect.width).clamp(0.0, 1.0);
    let bottom = (rect.y + rect.height).clamp(0.0, 1.0);
    (right > x && bottom > y).then_some(NormalizedRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn intersect_normalized_rect(
    rect: NormalizedRect,
    bounds: NormalizedRect,
) -> Option<NormalizedRect> {
    let x = rect.x.max(bounds.x);
    let y = rect.y.max(bounds.y);
    let right = (rect.x + rect.width).min(bounds.x + bounds.width);
    let bottom = (rect.y + rect.height).min(bounds.y + bounds.height);
    (right > x && bottom > y).then_some(NormalizedRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn sanitize_app_veil_snapshot(mut snapshot: AppVeilSnapshot) -> AppVeilSnapshot {
    snapshot.windows = snapshot
        .windows
        .into_iter()
        .filter_map(|window| {
            let frame = sanitize_normalized_rect(window.frame)?;
            let visible_fragments = window
                .visible_fragments
                .into_iter()
                .filter_map(sanitize_normalized_rect)
                .filter_map(|fragment| intersect_normalized_rect(fragment, frame))
                .collect::<Vec<_>>();
            (!visible_fragments.is_empty()).then_some(AppVeilWindow {
                frame,
                visible_fragments,
            })
        })
        .collect();
    snapshot
}

fn should_publish_app_veil_snapshot(
    force: bool,
    published: Option<&AppVeilSnapshot>,
    current: &AppVeilSnapshot,
) -> bool {
    force || published != Some(current)
}

/// Serializes room connects against teardown.
///
/// `create_room` and `destroy_room` both bump the generation. A queued `CreateRoom` whose
/// generation is no longer current was superseded by a `destroy_room` (the call ended while
/// an earlier `DestroyRoom` was still running) and must not connect. `arm` installs the
/// cancel senders only if still current, atomically with that check, so a `destroy_room`
/// can never slip in between and miss the connect it should cancel.
///
/// Commands that can wait on the server for seconds during a call (a screen track republish,
/// spec 0007) take a [`ConnectGate::superseded`] receiver too, so a hang-up isn't queued
/// behind them.
#[derive(Debug, Default)]
pub(crate) struct ConnectGate {
    generation: u64,
    /// Generation of the last connect that armed: the call in progress while it is current.
    armed: u64,
    cancel_connect: Vec<oneshot::Sender<()>>,
}

impl ConnectGate {
    /// New connect attempt; returns its generation. Supersedes and cancels any in-flight
    /// connect, so a stale call can't hold the command loop until it times out.
    pub(crate) fn begin(&mut self) -> u64 {
        self.invalidate();
        self.generation
    }

    /// Supersedes any queued or in-flight connect and cancels the in-flight one.
    pub(crate) fn invalidate(&mut self) {
        self.generation += 1;
        // Dropping the senders resolves the matching cancel receivers.
        self.cancel_connect.clear();
    }

    /// Installs cancel senders for `generation` if it is still current. Returns false
    /// (and drops the senders, i.e. cancels) when it was superseded.
    pub(crate) fn arm(&mut self, generation: u64, senders: Vec<oneshot::Sender<()>>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.armed = generation;
        self.cancel_connect = senders;
        true
    }

    /// Resolves once the call in progress is superseded by `destroy_room` or a new connect,
    /// at once if it already is, so a slow command can give way to the `DestroyRoom` or
    /// `CreateRoom` queued behind it.
    pub(crate) fn superseded(&mut self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        if self.armed == self.generation {
            // Drop the senders of finished commands and connects.
            self.cancel_connect.retain(|tx| !tx.is_closed());
            self.cancel_connect.push(tx);
        }
        rx
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }
}

pub struct CreateRoomParams {
    /// Call this room belongs to; echoed in `UserEvent::CreateRoomResult`.
    pub call_id: socket_lib::CallId,
    pub token: String,
    pub video_token: String,
    pub event_loop_proxy: EventLoopProxy<UserEvent>,
    pub mixer: audio::mixer::MixerHandle,
    pub sample_rate: u32,
    pub sample_rx: mpsc::UnboundedReceiver<Vec<i16>>,
    pub audio_processor: SharedProcessor,
    pub noise_cancellation_enabled: Arc<AtomicBool>,
    pub start_mic_on_call: bool,
    pub start_camera_on_call: bool,
    /// The "Use HEVC for screen sharing" setting (spec 0007).
    pub screen_share_hevc: bool,
}

/// A change in who can watch our screen share, seen on the video room (spec 0007).
#[derive(Debug)]
enum ScreenShareWatcherEvent {
    /// A watcher joined or changed its announcement: `announced` is its HEVC support, `None`
    /// when it announced nothing.
    Seen {
        identity: String,
        announced: Option<bool>,
    },
    /// A watcher that hasn't announced anything published a track. A Hopp client announces
    /// before it publishes, so this is an older client or a browser.
    PublishedUnannounced {
        identity: String,
    },
    Left {
        identity: String,
    },
    /// A watcher that joined without announcing had its `HEVC_ANNOUNCEMENT_GRACE`.
    AnnouncementOverdue {
        identity: String,
        token: u64,
    },
}

enum RoomServiceCommand {
    CreateRoom(CreateRoomParams, u64),
    PublishCursorPosition(f64, f64, bool),
    PublishControllerCursorEnabled(bool),
    DestroyRoom,
    TickResponse(u128),
    PublishParticipantInControl(String),
    PublishDrawStart(DrawPathPoint),
    PublishDrawAddPoint(ClientPoint),
    PublishDrawEnd(ClientPoint),
    PublishDrawClearPaths(Vec<u64>),
    PublishDrawClearAllPaths,
    PublishDrawText(DrawTextData),
    PublishDrawingMode(DrawingMode),
    UnpublishAudioTrack,
    MuteAudioTrack,
    UnmuteAudioTrack,
    MuteCameraTrack,
    UnmuteCameraTrack,
    MuteScreenShareTrack,
    UnmuteScreenShareTrack,
    PublishMouseClick(MouseClickData),
    PublishKeystroke(KeystrokeData),
    PublishWheelEvent(WheelDelta),
    PublishAddToClipboard(AddToClipboardData),
    PublishPasteFromClipboard(PasteFromClipboardData),
    PublishClipboardData(ClipboardDataPayload),
    PublishClickAnimation(ClientPoint),
    /// Screen effect trigger (id only), published lossy on `TOPIC_EFFECT`.
    PublishEffect(&'static str),
    PublishAppVeilSnapshot {
        force: bool,
    },
    SetLocalLowBandwidth(bool),
    /// Re-publishes our low-bandwidth request (if any) for late joiners.
    RepublishBandwidthModeRequest,
    SetScreenCaptureSize(Option<(u32, u32)>),
    /// The "Use HEVC for screen sharing" setting changed.
    SetScreenShareHevc(bool),
    /// Announces our HEVC support again (the call room reconnected, maybe as a new session).
    AnnounceHevcSupport,
    /// A screen share watcher event from the video room of connect generation `u64`.
    ScreenShareWatcher(ScreenShareWatcherEvent, u64),
    /// The screen track `track_sid` didn't start encoding HEVC (spec 0007 B6).
    HevcEncodingFailed {
        track_sid: TrackSid,
        reason: String,
    },
}

impl std::fmt::Debug for RoomServiceCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateRoom { .. } => write!(f, "CreateRoom"),
            Self::PublishCursorPosition(..) => write!(f, "PublishCursorPosition"),
            Self::PublishControllerCursorEnabled(v) => {
                write!(f, "PublishControllerCursorEnabled({v})")
            }
            Self::DestroyRoom => write!(f, "DestroyRoom"),
            Self::TickResponse(v) => write!(f, "TickResponse({v})"),
            Self::PublishParticipantInControl(v) => {
                write!(f, "PublishParticipantInControl({v})")
            }
            Self::PublishDrawStart(..) => write!(f, "PublishDrawStart"),
            Self::PublishDrawAddPoint(..) => write!(f, "PublishDrawAddPoint"),
            Self::PublishDrawEnd(..) => write!(f, "PublishDrawEnd"),
            Self::PublishDrawClearPaths(..) => write!(f, "PublishDrawClearPaths"),
            Self::PublishDrawClearAllPaths => write!(f, "PublishDrawClearAllPaths"),
            Self::PublishDrawText(..) => write!(f, "PublishDrawText"),
            Self::PublishDrawingMode(..) => write!(f, "PublishDrawingMode"),
            Self::UnpublishAudioTrack => write!(f, "UnpublishAudioTrack"),
            Self::MuteAudioTrack => write!(f, "MuteAudioTrack"),
            Self::UnmuteAudioTrack => write!(f, "UnmuteAudioTrack"),
            Self::MuteCameraTrack => write!(f, "MuteCameraTrack"),
            Self::UnmuteCameraTrack => write!(f, "UnmuteCameraTrack"),
            Self::MuteScreenShareTrack => write!(f, "MuteScreenShareTrack"),
            Self::UnmuteScreenShareTrack => write!(f, "UnmuteScreenShareTrack"),
            Self::PublishMouseClick(..) => write!(f, "PublishMouseClick"),
            Self::PublishKeystroke(..) => write!(f, "PublishKeystroke"),
            Self::PublishWheelEvent(..) => write!(f, "PublishWheelEvent"),
            Self::PublishAddToClipboard(..) => write!(f, "PublishAddToClipboard"),
            Self::PublishPasteFromClipboard(..) => write!(f, "PublishPasteFromClipboard"),
            Self::PublishClipboardData(..) => write!(f, "PublishClipboardData"),
            Self::PublishClickAnimation(..) => write!(f, "PublishClickAnimation"),
            Self::PublishEffect(id) => write!(f, "PublishEffect({id})"),
            Self::PublishAppVeilSnapshot { force } => {
                write!(f, "PublishAppVeilSnapshot {{ force: {force} }}")
            }
            Self::SetLocalLowBandwidth(v) => write!(f, "SetLocalLowBandwidth({v})"),
            Self::RepublishBandwidthModeRequest => write!(f, "RepublishBandwidthModeRequest"),
            Self::SetScreenCaptureSize(v) => write!(f, "SetScreenCaptureSize({v:?})"),
            Self::SetScreenShareHevc(v) => write!(f, "SetScreenShareHevc({v})"),
            Self::AnnounceHevcSupport => write!(f, "AnnounceHevcSupport"),
            Self::ScreenShareWatcher(event, generation) => {
                write!(f, "ScreenShareWatcher({event:?}, {generation})")
            }
            Self::HevcEncodingFailed { track_sid, reason } => {
                write!(f, "HevcEncodingFailed({track_sid}, {reason})")
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RoomServiceError {
    #[error("Failed to create room: {0}")]
    CreateRoom(String),
}

#[derive(Debug, Clone)]
struct RemoteScreenShare {
    buffer: Arc<std::sync::Mutex<Option<Arc<VideoBufferManager>>>>,
    stop_tx: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<()>>>>,
    publisher_identity: Arc<std::sync::Mutex<Option<String>>>,
    app_veil_snapshot: Arc<std::sync::Mutex<Option<(String, AppVeilSnapshot)>>>,
    shown: Arc<std::sync::Mutex<ShownScreenShareTrack>>,
}

/// The remote screen track a viewer shows, so a sharer replacing its track (a codec switch,
/// spec 0007) keeps the window open and only swaps the stream.
#[derive(Debug, Default)]
struct ShownScreenShareTrack {
    sid: Option<TrackSid>,
    /// Redraw channel of the window showing the share; a replacement stream reuses it.
    redraw_tx: Option<std::sync::mpsc::Sender<RedrawCommand>>,
    /// The shown track was unpublished while live: the id of the wait for its replacement.
    awaiting_replacement: Option<u64>,
    next_wait: u64,
}

impl ShownScreenShareTrack {
    /// No track is shown anymore.
    fn forget(&mut self) {
        self.sid = None;
        self.redraw_tx = None;
        self.awaiting_replacement = None;
    }
}

/*
 * This struct is used for handling room events and functions
 * from a thread in the async runtime.
 */
#[derive(Debug)]
pub(crate) struct RoomServiceInner {
    // TODO: See if we can use a sync::Mutex instead of tokio::sync::Mutex
    pub(crate) room: Mutex<Option<Room>>,
    pub(crate) video_room: Mutex<Option<Room>>,
    buffer_source: std::sync::Mutex<Option<NativeVideoSource>>,
    camera_buffer_source: std::sync::Mutex<Option<NativeVideoSource>>,
    camera_track: std::sync::Mutex<Option<LocalVideoTrack>>,
    screen_share_track: std::sync::Mutex<Option<LocalVideoTrack>>,
    participants: Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>>,
    remote_screen_share: RemoteScreenShare,
    pub(crate) stats: std::sync::RwLock<crate::livekit::stats::RoomStats>,
    pub(crate) video_health_summary: std::sync::Mutex<crate::livekit::stats::VideoHealthSummary>,
    connection_quality: Arc<std::sync::Mutex<Option<ConnectionQuality>>>,
    connect_gate: std::sync::Mutex<ConnectGate>,
    snapshot_sender: SnapshotSender,
    /// Pushes state into the native windows (e.g. the screen-share window).
    event_loop_proxy: EventLoopProxy<UserEvent>,
    app_veil_snapshot: std::sync::Mutex<Option<AppVeilSnapshot>>,
    published_app_veil_snapshot: std::sync::Mutex<Option<AppVeilSnapshot>>,
    bandwidth: std::sync::Mutex<BandwidthNegotiation>,
    /// Output size of the active screen capture, `None` when not sharing.
    capture_size: std::sync::Mutex<Option<(u32, u32)>>,
    /// Codec of our screen track (spec 0007). Used on the command task, which also republishes
    /// the track, and while connecting.
    screen_share_codec: std::sync::Mutex<ScreenShareCodecChoice>,
}

impl RoomServiceInner {
    async fn clear(&self) {
        {
            let mut inner_room = self.room.lock().await;
            if let Some(room) = inner_room.take() {
                if let Err(e) = room.close().await {
                    log::error!("RoomServiceInner::clear: Failed to close room: {e:?}");
                }
            }
        }
        {
            let mut inner_video_room = self.video_room.lock().await;
            if let Some(video_room) = inner_video_room.take() {
                if let Err(e) = video_room.close().await {
                    log::error!("RoomServiceInner::clear: Failed to close video room: {e:?}");
                }
            }
        }
        {
            self.buffer_source.lock().unwrap().take();
        }
        {
            self.camera_buffer_source.lock().unwrap().take();
        }
        {
            self.camera_track.lock().unwrap().take();
        }
        {
            self.screen_share_track.lock().unwrap().take();
        }
        {
            let mut stop_tx_guard = self.remote_screen_share.stop_tx.lock().unwrap();
            if let Some(tx) = stop_tx_guard.take() {
                let _ = tx.send(());
            }
        }
        {
            self.participants.write().unwrap().clear();
        }
        {
            self.remote_screen_share.buffer.lock().unwrap().take();
            self.remote_screen_share
                .publisher_identity
                .lock()
                .unwrap()
                .take();
            self.remote_screen_share
                .app_veil_snapshot
                .lock()
                .unwrap()
                .take();
            self.remote_screen_share.shown.lock().unwrap().forget();
        }
        self.published_app_veil_snapshot.lock().unwrap().take();
        self.app_veil_snapshot.lock().unwrap().take();
        self.bandwidth.lock().unwrap().clear();
        self.capture_size.lock().unwrap().take();
        self.screen_share_codec.lock().unwrap().clear();
    }
}

/// Inserts a remote participant into the map if not already present.
/// Returns `true` if the participant was newly inserted.
fn insert_participant_if_absent(
    participants: &std::sync::RwLock<HashMap<String, ParticipantInfo>>,
    identity: &str,
    remote_participant: &livekit::participant::RemoteParticipant,
) -> bool {
    if remote_participant.identity().as_str().contains("video") {
        return false;
    }
    let mut guard = participants.write().unwrap();
    if guard.contains_key(identity) {
        return false;
    }
    guard.insert(
        identity.to_string(),
        ParticipantInfo::from_remote_participant(remote_participant),
    );
    true
}

/// Iterates over the remote participants in the room and inserts them into the
/// participants hashmap if not already present.
fn populate_participants_from_room(
    room: &Room,
    participants: &std::sync::RwLock<HashMap<String, ParticipantInfo>>,
) {
    for (_, remote_participant) in room.remote_participants() {
        let identity = remote_participant.identity().as_str().to_string();
        if insert_participant_if_absent(participants, &identity, &remote_participant) {
            log::info!(
                "populate_participants_from_room: participant added: {}",
                identity
            );
        }
    }
}

/// RoomService is a wrapper around the LiveKit room, on creation it
/// spawns a thread for handling async code.
/// It exposes a few functions for sending commands to the room service.
///
/// The room service is responsible for:
/// - Creating a room
/// - Destroying a room
/// - Publishing sharer location
/// - Publishing controller cursor enabled
/// - Publishing tick response
#[derive(Debug)]
pub struct RoomService {
    /* The runtime is used to spawn a thread for handling room events. */
    _async_runtime: tokio::runtime::Runtime,
    _audio_runtime: tokio::runtime::Runtime,
    service_command_tx: mpsc::UnboundedSender<RoomServiceCommand>,
    inner: Arc<RoomServiceInner>,
}

impl RoomService {
    /// Creates a new RoomService instance.
    ///
    /// This function initializes a multi-threaded async runtime and spawns a background
    /// task to handle room service commands. The service manages LiveKit room connections
    /// and provides methods for publishing data to the room.
    ///
    /// # Arguments
    ///
    /// * `livekit_server_url` - The URL of the LiveKit server to connect to
    ///
    /// # Returns
    ///
    /// * `Ok(RoomService)` - A new room service instance
    /// * `Err(std::io::Error)` - If the async runtime could not be created
    pub fn new(
        livekit_server_url: String,
        socket: socket_lib::SocketSender,
        event_loop_proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, std::io::Error> {
        livekit::webrtc::enable_zero_playout_delay().map_err(std::io::Error::other)?;

        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let participants = Arc::new(std::sync::RwLock::new(HashMap::new()));
        let snapshot_sender = SnapshotSender::new(socket, participants.clone());

        let inner = Arc::new(RoomServiceInner {
            room: Mutex::new(None),
            video_room: Mutex::new(None),
            buffer_source: std::sync::Mutex::new(None),
            camera_buffer_source: std::sync::Mutex::new(None),
            camera_track: std::sync::Mutex::new(None),
            screen_share_track: std::sync::Mutex::new(None),
            participants,
            remote_screen_share: RemoteScreenShare {
                buffer: Arc::new(std::sync::Mutex::new(None)),
                stop_tx: Arc::new(std::sync::Mutex::new(None)),
                publisher_identity: Arc::new(std::sync::Mutex::new(None)),
                app_veil_snapshot: Arc::new(std::sync::Mutex::new(None)),
                shown: Arc::new(std::sync::Mutex::new(ShownScreenShareTrack::default())),
            },
            stats: std::sync::RwLock::new(crate::livekit::stats::RoomStats::default()),
            video_health_summary: std::sync::Mutex::new(Default::default()),
            connection_quality: Arc::new(std::sync::Mutex::new(None)),
            connect_gate: std::sync::Mutex::new(ConnectGate::default()),
            snapshot_sender,
            event_loop_proxy,
            app_veil_snapshot: std::sync::Mutex::new(None),
            published_app_veil_snapshot: std::sync::Mutex::new(None),
            bandwidth: std::sync::Mutex::new(BandwidthNegotiation::default()),
            capture_size: std::sync::Mutex::new(None),
            screen_share_codec: std::sync::Mutex::new(new_screen_share_codec_choice()),
        });
        let audio_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("hopp-audio")
            .on_thread_start(|| {
                let _ = set_current_thread_priority(ThreadPriority::Max);
            })
            .enable_all()
            .build()?;
        let audio_handle = audio_runtime.handle().clone();

        let (service_command_tx, service_command_rx) = mpsc::unbounded_channel();
        async_runtime.spawn(room_service_commands(
            service_command_rx,
            service_command_tx.clone(),
            inner.clone(),
            livekit_server_url,
            audio_handle,
        ));

        Ok(Self {
            _async_runtime: async_runtime,
            _audio_runtime: audio_runtime,
            service_command_tx,
            inner,
        })
    }

    pub fn stats(&self) -> crate::livekit::stats::RoomStats {
        self.inner.stats.read().unwrap().clone()
    }

    pub fn connection_quality(&self) -> Option<ConnectionQuality> {
        *self.inner.connection_quality.lock().unwrap()
    }

    /// Creates a room, this will block until the room is created.
    ///
    /// This function will block until the room is created in the
    /// async runtime thread.
    ///
    /// # Arguments
    ///
    /// * `token` - The token to use to connect to the room
    /// * `event_loop_proxy` - The event loop proxy to send events to
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The room was created successfully
    /// * `Err(())` - The room was not created successfully
    pub fn create_room(&self, params: CreateRoomParams) -> Result<(), RoomServiceError> {
        log::info!("create_room");
        let generation = self.inner.connect_gate.lock().unwrap().begin();
        self.service_command_tx
            .send(RoomServiceCommand::CreateRoom(params, generation))
            .map_err(|e| RoomServiceError::CreateRoom(format!("Failed to send command: {e:?}")))?;
        log::info!("create_room: command dispatched (non-blocking)");
        Ok(())
    }

    /// Destroys the current room connection.
    pub fn destroy_room(&self) {
        log::info!("destroy_room");
        if let Ok(summary) = self.inner.video_health_summary.lock() {
            summary.log();
        }

        // Cancel any in-flight CreateRoom immediately, and make any CreateRoom still
        // queued behind a slow DestroyRoom skip itself when it is dequeued.
        self.inner.connect_gate.lock().unwrap().invalidate();

        let res = self
            .service_command_tx
            .send(RoomServiceCommand::DestroyRoom);
        if let Err(e) = res {
            log::error!("destroy_room: Failed to send command: {e:?}");
        }
    }

    /// Retrieves the native video source buffer for screen sharing.
    ///
    /// Returns `None` if the screen share track has not been published yet.
    pub fn get_buffer_source(&self) -> Option<NativeVideoSource> {
        log::info!("get_buffer_source");
        let inner = self.inner.buffer_source.lock().unwrap();
        inner.clone()
    }

    /// Publishes the sharer's cursor position to the room.
    ///
    /// This function sends the current cursor position of the person sharing their screen
    /// to all participants in the LiveKit room. The data is sent reliably using the
    /// "sharer_location" topic.
    ///
    /// # Arguments
    ///
    /// * `x` - The x-coordinate of the cursor position
    /// * `y` - The y-coordinate of the cursor position
    /// * `pointer` - Whether the pointer is visible (currently unused in the implementation)
    pub fn publish_cursor_position(&self, x: f64, y: f64, pointer: bool) {
        log::debug!("publish_cursor_position: {x:?}, {y:?}, {pointer:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishCursorPosition(x, y, pointer));
        if let Err(e) = res {
            log::error!("publish_cursor_position: Failed to send command: {e:?}");
        }
    }

    /// Publishes the remote control enabled status to the room.
    /// # Arguments
    ///
    /// * `enabled` - Whether remote control is enabled (true) or disabled (false)
    pub fn publish_controller_cursor_enabled(&self, enabled: bool) {
        log::info!("publish_controller_cursor_enabled: {enabled:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishControllerCursorEnabled(enabled));

        if let Err(e) = res {
            log::error!("publish_controller_cursor_enabled: Failed to send command: {e:?}");
        }
    }

    /// This was used for latency measurement, needs to
    /// be integrated properly for production usage.
    pub fn tick_response(&self, time: u128) {
        log::info!("tick_response: {time:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::TickResponse(time));
        if let Err(e) = res {
            log::error!("tick_response: Failed to send command: {e:?}");
        }
    }

    /// Returns a list of all non-local participants from the participants map.
    pub fn get_participants(&self) -> Vec<ParticipantData> {
        let guard = self.inner.participants.read().unwrap();
        guard
            .iter()
            .filter(|(key, _)| *key != "local")
            .map(|(identity, info)| ParticipantData {
                name: info.name().to_string(),
                identity: identity.clone(),
            })
            .collect()
    }

    /// Publishes controller controls to the room.
    pub fn publish_participant_in_control(&self, participant: String) {
        log::info!("publish_participant_in_control: {participant:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishParticipantInControl(participant));
        if let Err(e) = res {
            log::error!("publish_participant_in_control: Failed to send command: {e:?}");
        }
    }

    pub fn publish_draw_start(&self, point: DrawPathPoint) {
        log::debug!("publish_draw_start: {:?}", point);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawStart(point));
        if let Err(e) = res {
            log::error!("publish_draw_start: Error sending command: {e:?}");
        }
    }

    pub fn publish_draw_add_point(&self, point: ClientPoint) {
        log::debug!("publish_draw_add_point: {:?}", point);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawAddPoint(point));
        if let Err(e) = res {
            log::error!("publish_draw_add_point: Error sending command: {e:?}");
        }
    }

    pub fn publish_draw_end(&self, point: ClientPoint) {
        log::debug!("publish_draw_end: {:?}", point);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawEnd(point));
        if let Err(e) = res {
            log::error!("publish_draw_end: Error sending command: {e:?}");
        }
    }

    pub fn publish_draw_clear_paths(&self, path_ids: Vec<u64>) {
        log::debug!("publish_draw_clear_paths: {:?}", path_ids);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawClearPaths(path_ids));
        if let Err(e) = res {
            log::error!("publish_draw_clear_paths: Error sending command: {e:?}");
        }
    }

    pub fn publish_draw_clear_all_paths(&self) {
        log::debug!("publish_draw_clear_all_paths");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawClearAllPaths);
        if let Err(e) = res {
            log::error!("publish_draw_clear_all_paths: Error sending command: {e:?}");
        }
    }

    pub fn publish_draw_text(&self, data: DrawTextData) {
        log::debug!("publish_draw_text: {:?}", data);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawText(data));
        if let Err(e) = res {
            log::error!("publish_draw_text: Error sending command: {e:?}");
        }
    }

    pub fn publish_drawing_mode(&self, mode: DrawingMode) {
        log::debug!("publish_drawing_mode: {:?}", mode);
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishDrawingMode(mode));
        if let Err(e) = res {
            log::error!("publish_drawing_mode: Error sending command: {e:?}");
        }
    }

    /// Current low-bandwidth mode state, for windows opened mid-call.
    pub fn bandwidth_mode_state(&self) -> socket_lib::BandwidthModeState {
        bandwidth_mode_state(&self.inner)
    }

    /// Requests (or withdraws our request for) low-bandwidth mode in the current call.
    pub fn set_local_low_bandwidth(&self, enabled: bool) {
        log::info!("set_local_low_bandwidth: {enabled}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::SetLocalLowBandwidth(enabled));
        if let Err(e) = res {
            log::error!("set_local_low_bandwidth: Error sending command: {e:?}");
        }
    }

    /// The "Use HEVC for screen sharing" setting changed (spec 0007): announces it, and turning
    /// it off switches a share in progress to H.264.
    pub fn set_screen_share_hevc(&self, enabled: bool) {
        log::info!("set_screen_share_hevc: {enabled}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::SetScreenShareHevc(enabled));
        if let Err(e) = res {
            log::error!("set_screen_share_hevc: Error sending command: {e:?}");
        }
    }

    /// Sets the output size of the active screen capture (`None` when sharing stops),
    /// so the screen share encoding can follow the bandwidth mode.
    pub fn set_screen_capture_size(&self, size: Option<(u32, u32)>) {
        log::info!("set_screen_capture_size: {size:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::SetScreenCaptureSize(size));
        if let Err(e) = res {
            log::error!("set_screen_capture_size: Error sending command: {e:?}");
        }
    }

    /// Unpublishes the audio track from the room.
    pub fn unpublish_audio_track(&self) {
        log::info!("unpublish_audio_track");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::UnpublishAudioTrack);
        if let Err(e) = res {
            log::error!("unpublish_audio_track: Failed to send command: {e:?}");
        }
    }

    /// Mutes the audio track.
    pub fn mute_audio_track(&self) {
        log::info!("mute_audio_track");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::MuteAudioTrack);
        if let Err(e) = res {
            log::error!("mute_audio_track: Failed to send command: {e:?}");
        }
    }

    pub fn mute_camera_track(&self) {
        log::info!("mute_camera_track");
        {
            let mut participants = self.inner.participants.write().unwrap();
            if let Some(info) = participants.get_mut("local") {
                info.camera_buffers().set_inactive(true);
            }
        }
        if let Err(e) = self
            .service_command_tx
            .send(RoomServiceCommand::MuteCameraTrack)
        {
            log::error!("mute_camera_track: Failed to send command: {e:?}");
        }
    }

    pub fn unmute_camera_track(&self) {
        log::info!("unmute_camera_track");
        {
            let mut participants = self.inner.participants.write().unwrap();
            if let Some(info) = participants.get_mut("local") {
                info.camera_buffers().set_inactive(false);
            }
        }
        if let Err(e) = self
            .service_command_tx
            .send(RoomServiceCommand::UnmuteCameraTrack)
        {
            log::error!("unmute_camera_track: Failed to send command: {e:?}");
        }
    }

    pub fn mute_screen_share_track(&self) {
        log::info!("mute_screen_share_track");
        {
            let mut participants = self.inner.participants.write().unwrap();
            if let Some(info) = participants.get_mut("local") {
                info.set_is_screensharing(false);
            }
        }
        if let Err(e) = self
            .service_command_tx
            .send(RoomServiceCommand::MuteScreenShareTrack)
        {
            log::error!("mute_screen_share_track: Failed to send command: {e:?}");
        }
    }

    pub fn unmute_screen_share_track(&self) {
        log::info!("unmute_screen_share_track");
        {
            let mut participants = self.inner.participants.write().unwrap();
            if let Some(info) = participants.get_mut("local") {
                info.set_is_screensharing(true);
            }
        }
        if let Err(e) = self
            .service_command_tx
            .send(RoomServiceCommand::UnmuteScreenShareTrack)
        {
            log::error!("unmute_screen_share_track: Failed to send command: {e:?}");
        }
    }

    /// Publishes a mouse click event to the room.
    pub fn publish_mouse_click(&self, data: MouseClickData) {
        log::debug!("publish_mouse_click: {data:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishMouseClick(data));
        if let Err(e) = res {
            log::error!("publish_mouse_click: Failed to send command: {e:?}");
        }
    }

    /// Publishes a keystroke event to the room.
    pub fn publish_keystroke(&self, data: KeystrokeData) {
        log::debug!("publish_keystroke: {data:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishKeystroke(data));
        if let Err(e) = res {
            log::error!("publish_keystroke: Failed to send command: {e:?}");
        }
    }

    /// Publishes a wheel/scroll event to the room.
    pub fn publish_wheel_event(&self, data: WheelDelta) {
        log::debug!("publish_wheel_event: {data:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishWheelEvent(data));
        if let Err(e) = res {
            log::error!("publish_wheel_event: Failed to send command: {e:?}");
        }
    }

    /// Publishes an add to clipboard event to the room.
    pub fn publish_add_to_clipboard(&self, data: AddToClipboardData) {
        log::debug!("publish_add_to_clipboard");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishAddToClipboard(data));
        if let Err(e) = res {
            log::error!("publish_add_to_clipboard: Failed to send command: {e:?}");
        }
    }

    /// Publishes a paste from clipboard event to the room.
    pub fn publish_paste_from_clipboard(&self, data: PasteFromClipboardData) {
        log::debug!("publish_paste_from_clipboard");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishPasteFromClipboard(data));
        if let Err(e) = res {
            log::error!("publish_paste_from_clipboard: Failed to send command: {e:?}");
        }
    }

    /// Publishes clipboard data from sharer back to the requesting controller.
    pub fn publish_clipboard_data(&self, data: ClipboardDataPayload) {
        log::debug!("publish_clipboard_data");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishClipboardData(data));
        if let Err(e) = res {
            log::error!("publish_clipboard_data: Failed to send command: {e:?}");
        }
    }

    /// Publishes a screen effect trigger to the room (lossy, fire-and-forget).
    pub fn publish_effect(&self, id: &'static str) {
        log::debug!("publish_effect: {id}");
        if let Err(e) = self
            .service_command_tx
            .send(RoomServiceCommand::PublishEffect(id))
        {
            log::error!("publish_effect: Failed to send command: {e:?}");
        }
    }

    /// Publishes a click animation event to the room.
    pub fn publish_click_animation(&self, point: ClientPoint) {
        log::debug!("publish_click_animation: {point:?}");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::PublishClickAnimation(point));
        if let Err(e) = res {
            log::error!("publish_click_animation: Failed to send command: {e:?}");
        }
    }

    pub fn set_app_veil_snapshot(&self, snapshot: AppVeilSnapshot) {
        let mut current = self.inner.app_veil_snapshot.lock().unwrap();
        if current.as_ref() == Some(&snapshot) {
            return;
        }
        *current = Some(snapshot);
        drop(current);
        if let Err(error) = self
            .service_command_tx
            .send(RoomServiceCommand::PublishAppVeilSnapshot { force: false })
        {
            log::error!("set_app_veil_snapshot: failed to send command: {error:?}");
        }
    }

    pub fn app_veil_snapshot(&self) -> AppVeilSnapshot {
        let publisher = self
            .inner
            .remote_screen_share
            .publisher_identity
            .lock()
            .unwrap()
            .clone();
        let snapshot = self
            .inner
            .remote_screen_share
            .app_veil_snapshot
            .lock()
            .unwrap();
        match (publisher, snapshot.as_ref()) {
            (Some(publisher), Some((sender, snapshot)))
                if participant_identities_match(&publisher, sender) =>
            {
                snapshot.clone()
            }
            _ => AppVeilSnapshot::default(),
        }
    }

    /// Retrieves the camera video source buffer.
    pub fn get_camera_buffer_source(&self) -> Option<NativeVideoSource> {
        log::info!("get_camera_buffer_source");
        let inner = self.inner.camera_buffer_source.lock().unwrap();
        inner.clone()
    }

    /// Returns a shared reference to the participants map.
    pub fn participants(&self) -> Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>> {
        self.inner.participants.clone()
    }

    /// Creates and sets a camera buffer manager for the local participant.
    pub fn local_camera_buffer_manager(&self) -> Arc<VideoBufferManager> {
        let mut participants = self.inner.participants.write().unwrap();
        let info = participants
            .get_mut("local")
            .expect("local participant info not found"); // This should never happen.
        info.camera_buffers()
    }

    /// Returns the remote screen share buffer if available.
    pub fn screen_share_buffer(&self) -> Option<Arc<VideoBufferManager>> {
        let buffer = self.inner.remote_screen_share.buffer.lock().unwrap();
        buffer.clone()
    }

    /// Returns whether the local audio track is currently muted.
    pub fn is_audio_muted(&self) -> bool {
        if let Ok(participants) = self.inner.participants.read() {
            if let Some(local) = participants.get("local") {
                return local.muted();
            }
        }
        true
    }

    /// Builds a participants snapshot and sends it directly over the socket.
    pub fn send_participants_snapshot(&self) {
        self.inner.snapshot_sender.send_participants_snapshot();
    }

    /// Unmutes the audio track.
    pub fn unmute_audio_track(&self) {
        log::info!("unmute_audio_track");
        let res = self
            .service_command_tx
            .send(RoomServiceCommand::UnmuteAudioTrack);
        if let Err(e) = res {
            log::error!("unmute_audio_track: Failed to send command: {e:?}");
        }
    }
}

/// Handles room service commands in an async loop.
///
/// This function processes commands sent through the `service_rx` channel and executes
/// corresponding actions on the LiveKit room. It runs continuously until the channel
/// is closed or an unrecoverable error occurs.
///
/// # Arguments
///
/// * `service_rx` - Unbounded receiver for room service commands
/// * `tx` - Synchronous sender for command results (Success/Failure)
/// * `inner` - Shared reference to the room service inner state
///
/// # Commands Handled
///
/// * `CreateRoom` - Creates a new LiveKit room connection and sets up event handing.
///   If a room already exists, it will be closed first.
///
/// * `PublishTrack` - Publishes a video track. The video track is configured with
///   VP9 codec and adaptive bitrate based on width.
///
/// * `DestroyRoom` - Closes the current room connection and cleans up associated
///   resources including the buffer source.
///
/// * `PublishCursorPosition` - Publishes cursor position data to the room
///   with topic "sharer_location".
///
/// * `PublishControllerCursorEnabled` - Publishes remote control enable/disable
///   status to the room with topic "remote_control_enabled".
///
/// * `TickResponse` - Publishes timing data to the room with topic "tick_response".
///
/// # Error Handling
///
/// The function logs errors for individual command failures but continues processing
/// subsequent commands. Command results are sent back through the `tx` channel.
/// Room state validation is performed before executing commands that require an
/// active room connection.
async fn room_service_commands(
    mut service_rx: mpsc::UnboundedReceiver<RoomServiceCommand>,
    service_command_tx: mpsc::UnboundedSender<RoomServiceCommand>,
    inner: Arc<RoomServiceInner>,
    livekit_server_url: String,
    audio_handle: tokio::runtime::Handle,
) {
    let mut stats_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut audio_publisher: Option<AudioPublisher> = None;
    let mut last_cursor_publish = Instant::now();
    const CURSOR_THROTTLE: Duration = Duration::from_millis(10);

    while let Some(command) = service_rx.recv().await {
        log::debug!("room_service_commands: Received command {command:?}");
        match command {
            RoomServiceCommand::CreateRoom(
                CreateRoomParams {
                    call_id,
                    token,
                    video_token,
                    event_loop_proxy,
                    mixer,
                    sample_rate,
                    sample_rx,
                    audio_processor,
                    noise_cancellation_enabled,
                    start_mic_on_call,
                    start_camera_on_call,
                    screen_share_hevc,
                },
                generation,
            ) => {
                log::info!("room_service_commands: CreateRoom call_id={call_id}");
                let total_connect_start = Instant::now();

                // Create two oneshot channels so destroy_room() can cancel both
                // in-flight connects simultaneously by dropping all senders.
                let (cancel_tx_1, cancel_rx_1) = oneshot::channel::<()>();
                let (cancel_tx_2, cancel_rx_2) = oneshot::channel::<()>();
                let armed = inner
                    .connect_gate
                    .lock()
                    .unwrap()
                    .arm(generation, vec![cancel_tx_1, cancel_tx_2]);
                if !armed {
                    log::info!(
                        "room_service_commands: CreateRoom for call {call_id} superseded while queued, skipping"
                    );
                    continue;
                }

                inner.clear().await;
                log::info!("room_service_commands: Cleared previous room state");
                // Nobody shares after `clear`, so this can't ask for a republish.
                let _ = inner
                    .screen_share_codec
                    .lock()
                    .unwrap()
                    .set_setting(screen_share_hevc);

                log::info!(
                    "room_service_commands: Connecting to room and video room in parallel (url={livekit_server_url}, signal_timeout={}s, setup_timeout={}s)",
                    SIGNAL_CONNECT_TIMEOUT.as_secs(),
                    ROOM_SETUP_TIMEOUT.as_secs()
                );
                let url = livekit_server_url.clone();
                let connect_start = Instant::now();
                let inner_clone = inner.clone();

                let connect_fut = tokio::time::timeout(ROOM_SETUP_TIMEOUT, async {
                    let mut room_options = RoomOptions::default();
                    room_options.dynacast = true;
                    room_options.connect_timeout = SIGNAL_CONNECT_TIMEOUT;
                    let phase_start = Instant::now();
                    let (room, rx) =
                        Room::connect(&url, &token, room_options)
                            .await
                            .map_err(|e| {
                                log::error!(
                                    "room_service_commands: Room::connect failed after {}ms: {e:?}",
                                    phase_start.elapsed().as_millis()
                                );
                                format!("{e:?}")
                            })?;
                    log::info!(
                        "room_service_commands: Room::connect (signal + ICE) took {}ms",
                        phase_start.elapsed().as_millis()
                    );
                    // As early as possible: a sharer waits only `HEVC_ANNOUNCEMENT_GRACE` for
                    // it after seeing us join. Not awaited, so it doesn't delay the call.
                    tokio::spawn(announce_hevc_support(
                        room.local_participant(),
                        screen_share_hevc,
                    ));
                    let phase_start = Instant::now();
                    let denoiser =
                        match crate::audio::denoiser::Denoiser::new(noise_cancellation_enabled) {
                            Ok(d) => {
                                log::info!(
                                    "DTLN denoiser initialized (tract) in {}ms",
                                    phase_start.elapsed().as_millis()
                                );
                                Some(d)
                            }
                            Err(e) => {
                                log::error!("Denoiser init failed, continuing without: {e}");
                                None
                            }
                        };
                    let publisher = AudioPublisher::publish(
                        &room,
                        sample_rate,
                        sample_rx,
                        audio_processor,
                        &audio_handle,
                        denoiser,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                    if !start_mic_on_call {
                        log::info!("room_service: start_mic_on_call=false, muting audio track");
                        publisher.mute();
                    }

                    // Publish camera track (muted) — non-fatal
                    let camera_source = NativeVideoSource::new(
                        VideoResolution {
                            width: 1280,
                            height: 720,
                        },
                        false,
                    );
                    let camera_track = LocalVideoTrack::create_video_track(
                        CAMERA_TRACK_NAME,
                        RtcVideoSource::Native(camera_source.clone()),
                    );
                    camera_track.mute();
                    let phase_start = Instant::now();
                    let camera_result = room
                        .local_participant()
                        .publish_track(
                            LocalTrack::Video(camera_track.clone()),
                            TrackPublishOptions {
                                source: TrackSource::Camera,
                                video_codec: VideoCodec::H264,
                                simulcast: true,
                                video_encoding: Some(VideoEncoding {
                                    max_bitrate: CAMERA_MAX_BITRATE,
                                    max_framerate: CAMERA_MAX_FRAMERATE,
                                }),
                                ..Default::default()
                            },
                        )
                        .await;
                    if let Err(e) = camera_result {
                        log::error!(
                            "room_service_commands: Failed to publish camera track after {}ms: {e:?}",
                            phase_start.elapsed().as_millis()
                        );
                    } else {
                        log::info!(
                            "room_service_commands: Camera track published in {}ms",
                            phase_start.elapsed().as_millis()
                        );
                        end_video_keepalive(camera_source.clone(), camera_track.clone(), "camera");
                        *inner_clone.camera_buffer_source.lock().unwrap() = Some(camera_source);
                        *inner_clone.camera_track.lock().unwrap() = Some(camera_track);
                    }

                    Ok::<_, String>((room, rx, publisher))
                });
                // Uncomment below to test artificial delay for testing racing conditions.
                // let connect_fut = async {
                //     tokio::time::timeout(Duration::from_secs(5), async {
                //         // Artificial delay for testing — set above timeout to trigger error handling.
                //         tokio::time::sleep(Duration::from_secs(6)).await;
                //         Room::connect(&url, &token, RoomOptions::default()).await
                //     })
                //     .await
                // };
                let inner_clone_video = inner.clone();
                let video_connect_fut = tokio::time::timeout(ROOM_SETUP_TIMEOUT, async {
                    let mut video_room_options = RoomOptions::default();
                    video_room_options.auto_subscribe = false;
                    video_room_options.connect_timeout = SIGNAL_CONNECT_TIMEOUT;
                    let phase_start = Instant::now();
                    let (video_room, video_rx) =
                        Room::connect(&url, &video_token, video_room_options)
                            .await
                            .map_err(|e| {
                                log::error!(
                            "room_service_commands: video Room::connect failed after {}ms: {e:?}",
                            phase_start.elapsed().as_millis()
                        );
                                format!("{e:?}")
                            })?;
                    log::info!(
                        "room_service_commands: video Room::connect (signal + ICE) took {}ms",
                        phase_start.elapsed().as_millis()
                    );
                    let phase_start = Instant::now();

                    // Publish screen share track (muted) — non-fatal
                    let screen_source = NativeVideoSource::new(
                        VideoResolution {
                            width: 1920,
                            height: 1080,
                        },
                        true,
                    );
                    // The codec for the watchers already here (spec 0007); a share starting
                    // later republishes the track if the pick changed by then.
                    let video_codec = {
                        let own_identity = video_room.local_participant().identity();
                        let mut codec_choice = inner_clone_video.screen_share_codec.lock().unwrap();
                        for participant in video_room.remote_participants().values() {
                            let identity = participant.identity();
                            if is_screen_share_watcher(identity.as_str(), own_identity.as_str()) {
                                let supports_hevc =
                                    screen_share_hevc::supports_hevc(&participant.attributes());
                                // Nobody shares yet, so this can't ask for a republish.
                                let _ = codec_choice.set_watcher(identity.as_str(), supports_hevc);
                            }
                        }
                        codec_choice.connected()
                    };
                    let screen_result = publish_screen_share_track(
                        &video_room.local_participant(),
                        &screen_source,
                        video_codec,
                        true,
                    )
                    .await;
                    match screen_result {
                        Err(e) => {
                            log::error!(
                                "room_service_commands: Failed to publish screen share track after {}ms: {e:?}",
                                phase_start.elapsed().as_millis()
                            );
                            inner_clone_video
                                .screen_share_codec
                                .lock()
                                .unwrap()
                                .track_lost();
                        }
                        Ok(screen_track) => {
                            log::info!(
                                "room_service_commands: Screen share track published as {video_codec:?} in {}ms",
                                phase_start.elapsed().as_millis()
                            );
                            end_video_keepalive(
                                screen_source.clone(),
                                screen_track.clone(),
                                "screen share",
                            );
                            *inner_clone_video.buffer_source.lock().unwrap() = Some(screen_source);
                            *inner_clone_video.screen_share_track.lock().unwrap() =
                                Some(screen_track);
                        }
                    }

                    Ok::<_, String>((video_room, video_rx))
                });

                // Run both connects concurrently. Each branch is independently
                // cancellable: dropping the corresponding sender in
                // cancel_connect resolves cancel_rx, causing that branch to
                // return None.
                let (regular_outcome, video_outcome) = tokio::join!(
                    async {
                        tokio::select! {
                            _ = cancel_rx_1 => {
                                log::info!(
                                    "room_service_commands: CreateRoom cancelled during regular room connect after {}ms",
                                    connect_start.elapsed().as_millis()
                                );
                                None
                            }
                            result = connect_fut => Some(result),
                        }
                    },
                    async {
                        tokio::select! {
                            _ = cancel_rx_2 => {
                                log::info!(
                                    "room_service_commands: CreateRoom cancelled during video room connect after {}ms",
                                    connect_start.elapsed().as_millis()
                                );
                                None
                            }
                            result = video_connect_fut => Some(result),
                        }
                    },
                );

                // If either connect was cancelled, clean up any room that did
                // manage to connect before the cancellation was observed.
                // This will only happen when CallEnd is called during waiting for the rooms to connect
                // on timeouts we get Ok(Err) not None.
                if regular_outcome.is_none() || video_outcome.is_none() {
                    if let Some(Ok(Ok((room, _, _)))) = regular_outcome {
                        let _ = room.close().await;
                    }
                    if let Some(Ok(Ok((video_room, _)))) = video_outcome {
                        let _ = video_room.close().await;
                    }
                    log::info!(
                        "room_service_commands: CreateRoom cancelled, cleaned up after {}ms",
                        total_connect_start.elapsed().as_millis()
                    );
                    continue;
                }

                // Both futures completed (not cancelled). Unwrap the Option layer.
                let regular_result = regular_outcome.unwrap();
                let video_result = video_outcome.unwrap();

                // The call may have ended just as the connects finished (after the cancel
                // select was decided). Don't keep rooms for a call that is over.
                if !inner.connect_gate.lock().unwrap().is_current(generation) {
                    if let Ok(Ok((room, _, _))) = regular_result {
                        let _ = room.close().await;
                    }
                    if let Ok(Ok((video_room, _))) = video_result {
                        let _ = video_room.close().await;
                    }
                    log::info!(
                        "room_service_commands: CreateRoom for call {call_id} superseded after connect, closed rooms"
                    );
                    continue;
                }

                // Handle regular room result
                let (room, rx, new_audio_publisher) = match regular_result {
                    Ok(Ok((room, rx, publisher))) => {
                        log::info!(
                            "room_service_commands: Regular room connected in {}ms",
                            connect_start.elapsed().as_millis()
                        );
                        (room, rx, publisher)
                    }
                    Ok(Err(e)) => {
                        log::error!(
                            "room_service_commands: Failed to connect to room after {}ms: {e:?}",
                            connect_start.elapsed().as_millis()
                        );
                        if let Ok(Ok((video_room, _))) = video_result {
                            let _ = video_room.close().await;
                        }
                        let _ = event_loop_proxy.send_event(UserEvent::CreateRoomResult(
                            call_id,
                            Err("Failed to connect to room".into()),
                        ));
                        continue;
                    }
                    Err(_) => {
                        log::error!(
                            "room_service_commands: Room connection timed out after {}ms",
                            connect_start.elapsed().as_millis()
                        );
                        if let Ok(Ok((video_room, _))) = video_result {
                            let _ = video_room.close().await;
                        }
                        let _ = event_loop_proxy.send_event(UserEvent::CreateRoomResult(
                            call_id,
                            Err("Room connection timed out".into()),
                        ));
                        continue;
                    }
                };
                audio_publisher = Some(new_audio_publisher);

                log::info!("room_service_commands: Connected to room");
                if let Some(task) = stats_task.take() {
                    task.abort();
                }
                stats_task = Some(tokio::spawn(crate::livekit::stats::stats_loop(
                    inner.clone(),
                )));
                log::info!("room_service_commands: Spawned stats task");
                let user_identity = room.local_participant().identity().as_str().to_string();
                let user_name = room.local_participant().name();
                log::info!("room_service_commands: Got user identity and name");
                {
                    let mut participants = inner.participants.write().unwrap();
                    participants.insert(
                        "local".to_string(),
                        ParticipantInfo::new(user_name, !start_mic_on_call, false),
                    );
                    if let Some(info) = participants.get_mut("local") {
                        info.camera_buffers().set_inactive(!start_camera_on_call);
                    }
                }
                log::info!(
                    "room_service_commands: Inserted local participant into participants map"
                );

                populate_participants_from_room(&room, &inner.participants);

                // Handle video room result — optional, failure is non-fatal.
                let mut video_rx_opt: Option<mpsc::UnboundedReceiver<RoomEvent>> = None;
                let video_participant_identity = match video_result {
                    Ok(Ok((video_room, video_rx))) => {
                        log::info!(
                            "room_service_commands: Video room connected in {}ms (total {}ms)",
                            connect_start.elapsed().as_millis(),
                            total_connect_start.elapsed().as_millis()
                        );
                        let identity = video_room
                            .local_participant()
                            .identity()
                            .as_str()
                            .to_string();
                        let mut inner_video_room = inner.video_room.lock().await;
                        *inner_video_room = Some(video_room);
                        video_rx_opt = Some(video_rx);
                        identity
                    }
                    Ok(Err(e)) => {
                        log::error!(
                            "room_service_commands: Failed to connect video room after {}ms (total {}ms): {e:?}",
                            connect_start.elapsed().as_millis(),
                            total_connect_start.elapsed().as_millis()
                        );
                        String::new()
                    }
                    Err(_) => {
                        log::error!(
                            "room_service_commands: Video room connection timed out after {}ms (total {}ms)",
                            connect_start.elapsed().as_millis(),
                            total_connect_start.elapsed().as_millis()
                        );
                        String::new()
                    }
                };
                if !video_participant_identity.is_empty() {
                    for (_, participant) in room.remote_participants() {
                        if participant.identity().as_str() == video_participant_identity {
                            for (_, publication) in participant.track_publications() {
                                publication.set_subscribed(false);
                            }
                        }
                    }
                }
                log::info!(
                    "room_service_commands: Finished room setup in {}ms",
                    total_connect_start.elapsed().as_millis()
                );
                {
                    let mut inner_room = inner.room.lock().await;
                    *inner_room = Some(room);
                }
                update_camera_quality(&inner).await;
                let snapshot = inner.snapshot_sender.build_snapshot();
                let _ =
                    event_loop_proxy.send_event(UserEvent::CreateRoomResult(call_id, Ok(snapshot)));
                tokio::spawn(handle_room_events(RoomEventContext {
                    receiver: rx,
                    event_loop_proxy,
                    user_identity,
                    video_participant_identity: video_participant_identity.clone(),
                    participants: inner.participants.clone(),
                    snapshot_sender: inner.snapshot_sender.clone(),
                    mixer,
                    remote_screen_share: inner.remote_screen_share.clone(),
                    connection_quality: inner.connection_quality.clone(),
                    audio_handle: audio_handle.clone(),
                    inner: inner.clone(),
                    service_command_tx: service_command_tx.clone(),
                }));
                log::info!("room_service_commands: Spawned handle_room_events");
                if let Some(video_rx) = video_rx_opt {
                    tokio::spawn(handle_video_room_events(
                        video_rx,
                        video_participant_identity,
                        service_command_tx.clone(),
                        generation,
                    ));
                    log::info!("room_service_commands: Spawned handle_video_room_events");
                }
            }
            RoomServiceCommand::DestroyRoom => {
                if let Some(task) = stats_task.take() {
                    task.abort();
                }

                // Unpublish audio
                if let Some(publisher) = audio_publisher.take() {
                    let inner_room = inner.room.lock().await;
                    if let Some(room) = inner_room.as_ref() {
                        publisher.unpublish(room).await;
                    }
                }
                // Unpublish camera
                let camera_track = inner.camera_track.lock().unwrap().take();
                if let Some(track) = camera_track {
                    let inner_room = inner.room.lock().await;
                    if let Some(room) = inner_room.as_ref() {
                        let _ = room.local_participant().unpublish_track(&track.sid()).await;
                    }
                }
                // Unpublish screen share
                let screen_share_track = inner.screen_share_track.lock().unwrap().take();
                if let Some(track) = screen_share_track {
                    let inner_video_room = inner.video_room.lock().await;
                    if let Some(room) = inner_video_room.as_ref() {
                        let _ = room.local_participant().unpublish_track(&track.sid()).await;
                    }
                }

                inner.clear().await;
            }
            RoomServiceCommand::PublishCursorPosition(x, y, _pointer) => {
                let now = Instant::now();
                if now.duration_since(last_cursor_publish) < CURSOR_THROTTLE {
                    continue;
                }
                last_cursor_publish = now;
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let res = local_participant
                    .publish_data(DataPacket {
                        payload: serde_json::to_vec(&ClientEvent::MouseMove(ClientPoint { x, y }))
                            .unwrap(),
                        reliable: true,
                        topic: Some(TOPIC_SHARER_LOCATION.to_string()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish cursor position: {e:?}");
                }
                log::debug!(
                    "Published cursor position with x: {x:?}, y: {y:?} to topic: {TOPIC_SHARER_LOCATION:?}"
                );
            }
            RoomServiceCommand::PublishControllerCursorEnabled(enabled) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let res = local_participant
                    .publish_data(DataPacket {
                        payload: serde_json::to_vec(&ClientEvent::RemoteControlEnabled(
                            RemoteControlEnabled { enabled },
                        ))
                        .unwrap(),
                        reliable: true,
                        topic: Some(TOPIC_REMOTE_CONTROL_ENABLED.to_string()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = res {
                    log::error!(
                        "room_service_commands: Failed to publish remote control change: {e:?}"
                    );
                }
            }
            RoomServiceCommand::TickResponse(time) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let res = local_participant
                    .publish_data(DataPacket {
                        payload: serde_json::to_vec(&ClientEvent::TickResponse(TickData { time }))
                            .unwrap(),
                        reliable: true,
                        topic: Some(TOPIC_TICK_RESPONSE.to_string()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish tick response: {e:?}");
                }
            }
            RoomServiceCommand::PublishParticipantInControl(participant) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let res = local_participant
                    .publish_data(DataPacket {
                        payload: participant.to_string().as_bytes().to_vec(),
                        reliable: true,
                        topic: Some(TOPIC_PARTICIPANT_IN_CONTROL.to_string()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = res {
                    log::error!(
                        "room_service_commands: Failed to publish participant in control: {e:?}"
                    );
                }
            }
            RoomServiceCommand::PublishDrawStart(point) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawStart(point);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish draw start: {e:?}");
                }
            }
            RoomServiceCommand::PublishDrawAddPoint(point) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawAddPoint(point);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: false,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish draw add point: {e:?}");
                }
            }
            RoomServiceCommand::PublishDrawEnd(point) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawEnd(point);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish draw end: {e:?}");
                }
            }
            RoomServiceCommand::PublishDrawClearPaths(path_ids) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };

                // Send individual DrawClearPath events for each path ID
                for path_id in path_ids {
                    let event = ClientEvent::DrawClearPath { path_id };
                    let payload = serde_json::to_vec(&event).unwrap();
                    let res = local_participant
                        .publish_data(DataPacket {
                            payload,
                            reliable: true,
                            topic: Some(TOPIC_DRAW.to_string()),
                            ..Default::default()
                        })
                        .await;

                    if let Err(e) = res {
                        log::error!(
                            "room_service_commands: Failed to publish draw clear path {}: {e:?}",
                            path_id
                        );
                    }
                }
            }
            RoomServiceCommand::PublishDrawClearAllPaths => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawClearAllPaths;
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!(
                        "room_service_commands: Failed to publish draw clear all paths: {e:?}"
                    );
                }
            }
            RoomServiceCommand::PublishDrawText(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawText(data);
                let payload = serde_json::to_vec(&event).unwrap();
                // Reliable (ordered) so a late position update can never
                // reopen text that was already committed or cancelled.
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish draw text: {e:?}");
                }
            }
            RoomServiceCommand::PublishDrawingMode(mode) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist");
                    continue;
                };
                let event = ClientEvent::DrawingMode(mode);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish drawing mode: {e:?}");
                }
            }
            RoomServiceCommand::SetLocalLowBandwidth(enabled) => {
                let (effective_changed, local_changed) = {
                    let mut bandwidth = inner.bandwidth.lock().unwrap();
                    let local_changed = bandwidth.local() != enabled;
                    (bandwidth.set_local(enabled), local_changed)
                };
                if local_changed {
                    publish_bandwidth_mode_request(&inner, enabled).await;
                }
                if effective_changed {
                    apply_bandwidth_mode(&inner).await;
                } else {
                    send_bandwidth_mode_state(&inner);
                }
            }
            RoomServiceCommand::RepublishBandwidthModeRequest => {
                let local = inner.bandwidth.lock().unwrap().local();
                if local {
                    publish_bandwidth_mode_request(&inner, true).await;
                }
            }
            RoomServiceCommand::SetScreenCaptureSize(size) => {
                *inner.capture_size.lock().unwrap() = size;
                if size.is_some() {
                    apply_screen_share_encoding(&inner);
                }
            }
            RoomServiceCommand::UnpublishAudioTrack => {
                if let Some(publisher) = audio_publisher.take() {
                    let inner_room = inner.room.lock().await;
                    if let Some(room) = inner_room.as_ref() {
                        publisher.unpublish(room).await;
                    }
                }
                log::info!("room_service_commands: Audio track unpublished");
            }
            RoomServiceCommand::MuteAudioTrack => {
                if let Some(publisher) = audio_publisher.as_ref() {
                    publisher.mute();
                    if let Ok(mut participants) = inner.participants.write() {
                        if let Some(local) = participants.get_mut("local") {
                            local.set_muted(true);
                        }
                    }

                    inner.snapshot_sender.send_participants_snapshot();

                    log::info!("room_service_commands: Audio track muted");
                } else {
                    log::warn!("room_service_commands: No audio track to mute");
                }
            }
            RoomServiceCommand::UnmuteAudioTrack => {
                if let Some(publisher) = audio_publisher.as_ref() {
                    publisher.unmute();
                    if let Ok(mut participants) = inner.participants.write() {
                        if let Some(local) = participants.get_mut("local") {
                            local.set_muted(false);
                        }
                    }

                    inner.snapshot_sender.send_participants_snapshot();

                    log::info!("room_service_commands: Audio track unmuted");
                } else {
                    log::warn!("room_service_commands: No audio track to unmute");
                }
            }
            RoomServiceCommand::MuteCameraTrack => {
                let track_updated = {
                    let camera_track = inner.camera_track.lock().unwrap();
                    if let Some(track) = camera_track.as_ref() {
                        track.mute();
                        true
                    } else {
                        false
                    }
                };
                if track_updated {
                    update_camera_quality(&inner).await;
                }
                log::info!("room_service_commands: Camera track muted");
            }
            RoomServiceCommand::UnmuteCameraTrack => {
                let track_updated = {
                    let camera_track = inner.camera_track.lock().unwrap();
                    if let Some(track) = camera_track.as_ref() {
                        track.unmute();
                        true
                    } else {
                        false
                    }
                };
                if track_updated {
                    update_camera_quality(&inner).await;
                }
                log::info!("room_service_commands: Camera track unmuted");
            }
            RoomServiceCommand::MuteScreenShareTrack => {
                inner.screen_share_codec.lock().unwrap().share_ended();
                if let Some(track) = inner.screen_share_track.lock().unwrap().as_ref() {
                    track.mute();
                }
                log::info!("room_service_commands: Screen share track muted");
            }
            RoomServiceCommand::UnmuteScreenShareTrack => {
                let republish = inner.screen_share_codec.lock().unwrap().share_started();
                let track = inner.screen_share_track.lock().unwrap().clone();
                if republish.is_some() {
                    // The new track is published unmuted.
                    republish_if_needed(&inner, republish, "share started", &service_command_tx)
                        .await;
                } else if let Some(track) = track {
                    track.unmute();
                    let codec = inner.screen_share_codec.lock().unwrap().published();
                    if let Some(codec) = codec {
                        spawn_screen_share_encoder_check(
                            &inner,
                            track,
                            codec,
                            Duration::ZERO,
                            &service_command_tx,
                        );
                    }
                } else {
                    // An earlier republish failed.
                    let codec = inner.screen_share_codec.lock().unwrap().connected();
                    republish_screen_share_track(
                        &inner,
                        codec,
                        "share started without a screen track",
                        &service_command_tx,
                    )
                    .await;
                }
                log::info!("room_service_commands: Screen share track unmuted");
            }
            RoomServiceCommand::SetScreenShareHevc(enabled) => {
                let republish = inner
                    .screen_share_codec
                    .lock()
                    .unwrap()
                    .set_setting(enabled);
                if let Some(local_participant) = room_local_participant(&inner).await {
                    unless_superseded(&inner, announce_hevc_support(local_participant, enabled))
                        .await;
                }
                republish_if_needed(&inner, republish, "HEVC turned off", &service_command_tx)
                    .await;
            }
            RoomServiceCommand::AnnounceHevcSupport => {
                let supported = inner.screen_share_codec.lock().unwrap().setting();
                if let Some(local_participant) = room_local_participant(&inner).await {
                    unless_superseded(&inner, announce_hevc_support(local_participant, supported))
                        .await;
                }
            }
            RoomServiceCommand::ScreenShareWatcher(event, generation) => {
                if !inner.connect_gate.lock().unwrap().is_current(generation) {
                    log::debug!(
                        "room_service_commands: stale screen share watcher event {event:?}"
                    );
                    continue;
                }
                let (republish, reason) = {
                    let mut codec_choice = inner.screen_share_codec.lock().unwrap();
                    match event {
                        ScreenShareWatcherEvent::Seen {
                            identity,
                            announced: Some(supports_hevc),
                        } => {
                            log::info!(
                                "room_service_commands: screen share watcher {identity} announces HEVC support: {supports_hevc}"
                            );
                            (
                                codec_choice.set_watcher(&identity, supports_hevc),
                                format!("{identity} can't watch HEVC"),
                            )
                        }
                        ScreenShareWatcherEvent::Seen {
                            identity,
                            announced: None,
                        } => {
                            if let Some(token) = codec_choice.watcher_unannounced(&identity) {
                                log::info!(
                                    "room_service_commands: screen share watcher {identity} joined without announcing HEVC support"
                                );
                                let service_command_tx = service_command_tx.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(HEVC_ANNOUNCEMENT_GRACE).await;
                                    let _ = service_command_tx.send(
                                        RoomServiceCommand::ScreenShareWatcher(
                                            ScreenShareWatcherEvent::AnnouncementOverdue {
                                                identity,
                                                token,
                                            },
                                            generation,
                                        ),
                                    );
                                });
                            }
                            (None, String::new())
                        }
                        ScreenShareWatcherEvent::PublishedUnannounced { identity } => {
                            log::info!(
                                "room_service_commands: screen share watcher {identity} publishes without announcing HEVC support"
                            );
                            (
                                codec_choice.set_watcher(&identity, false),
                                format!("{identity} publishes without announcing HEVC support"),
                            )
                        }
                        ScreenShareWatcherEvent::Left { identity } => {
                            codec_choice.remove_watcher(&identity);
                            (None, String::new())
                        }
                        ScreenShareWatcherEvent::AnnouncementOverdue { identity, token } => (
                            codec_choice.announcement_overdue(&identity, token),
                            format!(
                                "{identity} announced no HEVC support within {}ms",
                                HEVC_ANNOUNCEMENT_GRACE.as_millis()
                            ),
                        ),
                    }
                };
                republish_if_needed(&inner, republish, &reason, &service_command_tx).await;
            }
            RoomServiceCommand::HevcEncodingFailed { track_sid, reason } => {
                let current = inner
                    .screen_share_track
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(LocalVideoTrack::sid);
                if current.as_ref() != Some(&track_sid) {
                    log::info!(
                        "room_service_commands: ignoring the HEVC check of replaced track {track_sid}: {reason}"
                    );
                    continue;
                }
                log::error!(
                    "room_service_commands: HEVC encoding didn't start ({reason}); sharing H.264 for the rest of the call"
                );
                let republish = inner
                    .screen_share_codec
                    .lock()
                    .unwrap()
                    .hevc_encoding_failed();
                republish_if_needed(
                    &inner,
                    republish,
                    "HEVC encoding failed",
                    &service_command_tx,
                )
                .await;
            }
            RoomServiceCommand::PublishMouseClick(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist for PublishMouseClick");
                    continue;
                };

                let event = ClientEvent::MouseClick(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish mouse click: {e:?}");
                }
            }
            RoomServiceCommand::PublishKeystroke(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist for PublishKeystroke");
                    continue;
                };

                let event = ClientEvent::Keystroke(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish keystroke: {e:?}");
                }
            }
            RoomServiceCommand::PublishWheelEvent(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist for PublishWheelEvent");
                    continue;
                };

                let event = ClientEvent::WheelEvent(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish wheel event: {e:?}");
                }
            }
            RoomServiceCommand::PublishAddToClipboard(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!(
                        "room_service_commands: Room doesn't exist for PublishAddToClipboard"
                    );
                    continue;
                };

                let event = ClientEvent::AddToClipboard(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish add to clipboard: {e:?}");
                }
            }
            RoomServiceCommand::PublishPasteFromClipboard(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!(
                        "room_service_commands: Room doesn't exist for PublishPasteFromClipboard"
                    );
                    continue;
                };

                let event = ClientEvent::PasteFromClipboard(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!(
                        "room_service_commands: Failed to publish paste from clipboard: {e:?}"
                    );
                }
            }
            RoomServiceCommand::PublishClipboardData(data) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!(
                        "room_service_commands: Room doesn't exist for PublishClipboardData"
                    );
                    continue;
                };

                let event = ClientEvent::ClipboardData(data);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: None,
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish clipboard data: {e:?}");
                }
            }
            RoomServiceCommand::PublishClickAnimation(point) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!(
                        "room_service_commands: Room doesn't exist for PublishClickAnimation"
                    );
                    continue;
                };

                let event = ClientEvent::ClickAnimation(point);
                let payload = serde_json::to_vec(&event).unwrap();
                let res = local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_DRAW.to_string()),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish click animation: {e:?}");
                }
            }
            RoomServiceCommand::PublishEffect(id) => {
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist for PublishEffect");
                    continue;
                };
                // Lossy: a lost effect is harmless, and it must not queue in front of
                // keystrokes on the reliable channel.
                let res = local_participant
                    .publish_data(DataPacket {
                        payload: encode_effect_packet(id),
                        reliable: false,
                        topic: Some(TOPIC_EFFECT.to_string()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = res {
                    log::error!("room_service_commands: Failed to publish effect: {e:?}");
                }
            }
            RoomServiceCommand::PublishAppVeilSnapshot { force } => {
                let Some(snapshot) = inner.app_veil_snapshot.lock().unwrap().clone() else {
                    continue;
                };
                if !should_publish_app_veil_snapshot(
                    force,
                    inner.published_app_veil_snapshot.lock().unwrap().as_ref(),
                    &snapshot,
                ) {
                    continue;
                }
                let payload = match serde_json::to_vec(&snapshot) {
                    Ok(payload) => payload,
                    Err(error) => {
                        log::error!(
                            "room_service_commands: Failed to serialize App Veil snapshot: {error:?}"
                        );
                        continue;
                    }
                };
                let Some(local_participant) = room_local_participant(&inner).await else {
                    log::warn!("room_service_commands: Room doesn't exist for App Veil snapshot");
                    continue;
                };
                match local_participant
                    .publish_data(DataPacket {
                        payload,
                        reliable: true,
                        topic: Some(TOPIC_APP_VEIL.to_string()),
                        ..Default::default()
                    })
                    .await
                {
                    Ok(()) => {
                        *inner.published_app_veil_snapshot.lock().unwrap() = Some(snapshot);
                    }
                    Err(error) => log::error!(
                        "room_service_commands: Failed to publish App Veil snapshot: {error:?}"
                    ),
                }
            }
        }
    }
}

/// Represents a 2D point with floating-point coordinates.
///
/// This structure is used to represent cursor positions, mouse coordinates,
/// and other 2D locations within the room service.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct ClientPoint {
    /// The x-coordinate of the point
    pub x: f64,
    /// The y-coordinate of the point
    pub y: f64,
}

/// Represents a drawing path point with both coordinates and path identifier.
///
/// This structure combines a 2D point with a path ID to track which drawing
/// path the point belongs to, enabling multiple simultaneous drawing paths.
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct DrawPathPoint {
    /// The 2D coordinates of the point
    pub point: ClientPoint,
    /// The unique identifier for the drawing path this point belongs to
    pub path_id: u64,
}

/// Full state of a text annotation typed in drawing mode.
///
/// Every update carries the complete text, so updates are idempotent. The
/// `path_id` shares the id space of drawing paths, so `DrawClearPath` and
/// `DrawClearAllPaths` remove text the same way they remove strokes.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DrawTextData {
    /// The unique identifier shared with drawing paths
    pub path_id: u64,
    /// Top-left anchor of the text in normalized coordinates
    pub point: ClientPoint,
    /// The complete text content
    pub text: String,
    /// False while the text is being typed, true once it is placed
    pub committed: bool,
}

/// Contains data for mouse click events.
///
/// This structure captures all the information needed to represent a mouse click,
/// including position, button information, modifier keys, and click state.
#[derive(Debug, Serialize, Deserialize)]
pub struct MouseClickData {
    /// The x-coordinate where the click occurred
    pub x: f64,
    /// The y-coordinate where the click occurred
    pub y: f64,
    /// The mouse button that was clicked (0=left, 1=right, 2=middle)
    pub button: u32,
    /// The number of clicks (1=single, 2=double, etc.)
    pub clicks: u32,
    /// Whether the button is being pressed down (true) or released (false)
    pub down: bool,
    /// Whether the Shift key was held during the click
    pub shift: bool,
    /// Whether the Meta/Cmd key was held during the click
    pub meta: bool,
    /// Whether the Ctrl key was held during the click
    pub ctrl: bool,
    /// Whether the Alt key was held during the click
    pub alt: bool,
}

/// Contains data for mouse visibility events.
///
/// Contains data for mouse wheel scroll events.
///
/// This structure represents the scroll delta values for both horizontal
/// and vertical scrolling directions.
#[derive(Debug, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct WheelDelta {
    /// The horizontal scroll delta (positive = right, negative = left)
    pub deltaX: f64,
    /// The vertical scroll delta (positive = down, negative = up)
    pub deltaY: f64,
}

/// Contains data for keyboard input events.
///
/// This structure captures keyboard input including the keys pressed
/// and any modifier keys that were held during the keystroke.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeystrokeData {
    /// The key(s) that were pressed (as string representations)
    pub key: Vec<String>,
    /// Whether the Meta/Cmd key was held during the keystroke
    pub meta: bool,
    /// Whether the Ctrl key was held during the keystroke
    pub ctrl: bool,
    /// Whether the Shift key was held during the keystroke
    pub shift: bool,
    /// Whether the Alt key was held during the keystroke
    pub alt: bool,
    /// Whether the key is being pressed down (true) or released (false)
    pub down: bool,
}

/// Contains timing data for tick events.
///
/// This structure is used for synchronization and latency measurement
/// between room participants.
#[derive(Debug, Serialize, Deserialize)]
pub struct TickData {
    /// The timestamp value (typically in nanoseconds)
    pub time: u128,
}

/// Contains the remote control enabled/disabled state.
///
/// This structure is used to communicate whether remote control
/// functionality is currently enabled in the room.
#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteControlEnabled {
    /// Whether remote control is currently enabled
    pub enabled: bool,
}

/// Contains data for clipboard events.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AddToClipboardData {
    /// The text to be added to the clipboard
    pub is_copy: bool,
}

/// Contains data for clipboard events.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClipboardPayload {
    pub packet_id: u64,
    pub total_packets: u64,
    pub data: Vec<u8>,
}

/// Contains data for clipboard events.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PasteFromClipboardData {
    pub data: Option<ClipboardPayload>,
}

/// Clipboard data sent from sharer back to the requesting controller.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClipboardDataPayload {
    pub requester_sid: String,
    pub data: Option<ClipboardPayload>,
}

/// Settings specific to the Draw mode.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct DrawSettings {
    /// Whether drawn lines should be permanent or fade away after a while
    pub permanent: bool,
}

/// Drawing mode - specifies the type of drawing operation or disabled state.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(tag = "type", content = "settings")]
pub enum DrawingMode {
    /// Drawing mode is disabled
    Disabled,
    /// Standard drawing mode with its settings
    Draw(DrawSettings),
    /// Click animation mode
    ClickAnimation,
    /// Unknown state — used when a participant's drawing mode is not yet known
    Any,
}

/// Represents all possible client events that can be sent between room participants.
///
/// This enum defines the different types of events that can be transmitted through
/// the LiveKit room, including input events, cursor movements, and control messages.
/// Events are serialized as JSON with a `type` field and `payload` field containing
/// the event-specific data.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ClientEvent {
    /// Mouse cursor movement event from a remote controller
    MouseMove(ClientPoint),
    /// Mouse click event from a remote controller
    MouseClick(MouseClickData),
    /// Keyboard input event from a remote controller
    Keystroke(KeystrokeData),
    /// Mouse wheel scroll event from a remote controller
    WheelEvent(WheelDelta),
    /// Timing synchronization request
    Tick(TickData),
    /// Response to a timing synchronization request
    TickResponse(TickData),
    /// Remote control enabled/disabled status change
    RemoteControlEnabled(RemoteControlEnabled),
    /// Copy or cut command from a remote controller
    AddToClipboard(AddToClipboardData),
    /// Paste command from a remote controller
    PasteFromClipboard(PasteFromClipboardData),
    /// Clipboard data sent from sharer back to the requesting controller
    ClipboardData(ClipboardDataPayload),
    /// Drawing mode change event (disabled, draw, or click animation)
    DrawingMode(DrawingMode),
    /// Drawing started at a point with a path identifier
    DrawStart(DrawPathPoint),
    /// Add a point to the current in-progress drawing
    DrawAddPoint(ClientPoint),
    /// Drawing ended at a point
    DrawEnd(ClientPoint),
    /// Clear a specific drawing path
    DrawClearPath { path_id: u64 },
    /// Clear all drawing paths
    DrawClearAllPaths,
    /// Text annotation typed in drawing mode (full state)
    DrawText(DrawTextData),
    /// Click animation at a point
    ClickAnimation(ClientPoint),
}

fn start_remote_camera_stream(
    video_track: livekit::track::RemoteVideoTrack,
    participants: &Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>>,
    identity: &str,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
) {
    if let Err(e) = event_loop_proxy.send_event(UserEvent::OpenCamera) {
        log::error!("handle_room_events: Failed to send OpenCamera event: {e:?}");
    }
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let manager = {
        let mut guard = participants.write().unwrap();
        let info = guard.get_mut(identity).expect("Participant should exist");
        info.set_camera_stop_tx(stop_tx);
        info.camera_buffers()
    };
    tokio::spawn(process_video_stream(
        video_track,
        manager,
        stop_rx,
        identity.to_string(),
        true,
        None,
    ));
}

fn start_remote_screen_share_stream(
    video_track: livekit::track::RemoteVideoTrack,
    track_sid: TrackSid,
    remote_screen_share: &RemoteScreenShare,
    participants: &Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>>,
    participant_identity: &str,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
    snapshot_sender: &SnapshotSender,
) {
    // Held throughout, like when a share ends: a replacement wait timing out
    // (`await_screen_share_replacement`) can't interleave with this start.
    let mut shown = remote_screen_share.shown.lock().unwrap();
    if replace_remote_screen_share_stream(
        &mut shown,
        &video_track,
        &track_sid,
        remote_screen_share,
        participant_identity,
    ) {
        return;
    }
    {
        let mut stop_tx_guard = remote_screen_share.stop_tx.lock().unwrap();
        if let Some(tx) = stop_tx_guard.take() {
            log::info!("handle_room_events: Stopping existing screen share task");
            let _ = tx.send(());
        }
    }

    let manager = {
        let mut buffer_guard = remote_screen_share.buffer.lock().unwrap();
        if let Some(existing) = buffer_guard.as_ref() {
            existing.clone()
        } else {
            let new = Arc::new(VideoBufferManager::default());
            *buffer_guard = Some(new.clone());
            new
        }
    };

    *remote_screen_share.publisher_identity.lock().unwrap() =
        Some(participant_identity.to_string());
    let mut app_veil_snapshot = remote_screen_share.app_veil_snapshot.lock().unwrap();
    if app_veil_snapshot
        .as_ref()
        .is_some_and(|(sender, _)| !participant_identities_match(sender, participant_identity))
    {
        app_veil_snapshot.take();
    }
    drop(app_veil_snapshot);

    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    *remote_screen_share.stop_tx.lock().unwrap() = Some(stop_tx);

    let (redraw_tx, redraw_rx) =
        std::sync::mpsc::channel::<crate::window::screensharing_window::RedrawCommand>();
    let redraw_rx = std::sync::Arc::new(std::sync::Mutex::new(Some(redraw_rx)));

    tokio::spawn(process_video_stream(
        video_track,
        manager,
        stop_rx,
        format!("screenshare_{participant_identity}"),
        false,
        Some(redraw_tx.clone()),
    ));

    let sharer_identity = {
        let audio_identity = participant_identity
            .strip_suffix(":video")
            .map(|prefix| format!("{prefix}:audio"));
        if let Some(audio_id) = audio_identity {
            let guard = participants.read().unwrap();
            if guard.contains_key(&audio_id) {
                audio_id
            } else {
                participant_identity.to_string()
            }
        } else {
            participant_identity.to_string()
        }
    };

    {
        let mut participants_guard = participants.write().unwrap();
        if let Some(info) = participants_guard.get_mut(&sharer_identity) {
            info.set_is_screensharing(true);
        }
    }

    snapshot_sender.send_participants_snapshot();

    shown.sid = Some(track_sid);
    shown.redraw_tx = Some(redraw_tx.clone());
    shown.awaiting_replacement = None;

    if let Err(e) = event_loop_proxy.send_event(UserEvent::OpenScreenShareWindow {
        participants: participants.clone(),
        sharer_identity: Some(sharer_identity),
        redraw_rx: Some(redraw_rx),
        redraw_tx: Some(redraw_tx),
    }) {
        log::error!("handle_room_events: Failed to send OpenScreenShareWindow event: {e:?}");
    }
}

/// The sharer we show published a new screen track in place of the shown one (a codec switch,
/// spec 0007): feeds it into the same buffer and window. The window isn't reopened, focused or
/// reset, so its drawings and mode stay, and the last frame stays up until the new track's
/// first one. Returns false, changing nothing, when this isn't a replacement.
fn replace_remote_screen_share_stream(
    shown: &mut ShownScreenShareTrack,
    video_track: &livekit::track::RemoteVideoTrack,
    track_sid: &TrackSid,
    remote_screen_share: &RemoteScreenShare,
    participant_identity: &str,
) -> bool {
    let same_sharer = remote_screen_share
        .publisher_identity
        .lock()
        .unwrap()
        .as_deref()
        == Some(participant_identity);
    if !same_sharer {
        return false;
    }
    let replaced = shown.awaiting_replacement.is_some()
        || shown.sid.as_ref().is_some_and(|sid| sid != track_sid);
    let Some(redraw_tx) = shown.redraw_tx.clone().filter(|_| replaced) else {
        return false;
    };
    let Some(manager) = remote_screen_share.buffer.lock().unwrap().clone() else {
        return false;
    };
    log::info!(
        "handle_room_events: {participant_identity} replaced screen track {:?} with {track_sid}",
        shown.sid
    );
    shown.sid = Some(track_sid.clone());
    shown.awaiting_replacement = None;

    if let Some(tx) = remote_screen_share.stop_tx.lock().unwrap().take() {
        let _ = tx.send(());
    }
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    *remote_screen_share.stop_tx.lock().unwrap() = Some(stop_tx);
    tokio::spawn(process_video_stream(
        video_track.clone(),
        manager,
        stop_rx,
        format!("screenshare_{participant_identity}"),
        false,
        Some(redraw_tx),
    ));
    true
}

/// Whether `sid` from `identity` is the screen track we show (or, before its sid is known,
/// comes from the sharer we show).
fn is_shown_screen_share(
    remote_screen_share: &RemoteScreenShare,
    identity: &str,
    sid: &TrackSid,
) -> bool {
    let from_sharer = remote_screen_share
        .publisher_identity
        .lock()
        .unwrap()
        .as_deref()
        == Some(identity);
    from_sharer
        && remote_screen_share
            .shown
            .lock()
            .unwrap()
            .sid
            .as_ref()
            .is_none_or(|shown| shown == sid)
}

/// The share we show ended: stops its stream, closes its window and updates the participant
/// list.
fn end_remote_screen_share(
    remote_screen_share: &RemoteScreenShare,
    participants: &std::sync::RwLock<HashMap<String, ParticipantInfo>>,
    sharer_video_identity: &str,
    snapshot_sender: &SnapshotSender,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
) {
    let mut shown = remote_screen_share.shown.lock().unwrap();
    end_shown_remote_screen_share(
        &mut shown,
        remote_screen_share,
        participants,
        sharer_video_identity,
        snapshot_sender,
        event_loop_proxy,
    );
}

/// `end_remote_screen_share` with `shown` already locked. The lock is held throughout, so a
/// new share starting (`start_remote_screen_share_stream`) comes before or after the whole end:
/// lock order `shown`, then the other `RemoteScreenShare` fields and `participants`.
fn end_shown_remote_screen_share(
    shown: &mut ShownScreenShareTrack,
    remote_screen_share: &RemoteScreenShare,
    participants: &std::sync::RwLock<HashMap<String, ParticipantInfo>>,
    sharer_video_identity: &str,
    snapshot_sender: &SnapshotSender,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
) {
    if let Some(tx) = remote_screen_share.stop_tx.lock().unwrap().take() {
        let _ = tx.send(());
    }
    // Keep the buffer for reuse.
    remote_screen_share
        .publisher_identity
        .lock()
        .unwrap()
        .take();
    remote_screen_share.app_veil_snapshot.lock().unwrap().take();
    shown.forget();

    // Derive the audio participant identity to clear is_screensharing
    let audio_identity = sharer_video_identity
        .strip_suffix(":video")
        .map(|prefix| format!("{prefix}:audio"));
    if let Some(audio_id) = audio_identity {
        let mut participants_guard = participants.write().unwrap();
        if let Some(info) = participants_guard.get_mut(&audio_id) {
            info.set_is_screensharing(false);
        }
    }

    snapshot_sender.send_participants_snapshot();

    if let Err(e) = event_loop_proxy.send_event(UserEvent::CloseScreenShareWindow) {
        log::error!("handle_room_events: Failed to send CloseScreenShareWindow event: {e:?}");
    }
}

/// The screen track we show was unpublished while live. A sharer switching codec publishes the
/// new track right after (spec 0007), so the window stays open on the last frame while the
/// replacement is awaited; without one within `SCREEN_SHARE_REPLACEMENT_GRACE`, the share ends.
fn await_screen_share_replacement(
    remote_screen_share: &RemoteScreenShare,
    participants: &Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>>,
    sharer_video_identity: &str,
    snapshot_sender: &SnapshotSender,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
) {
    let wait = {
        let mut shown = remote_screen_share.shown.lock().unwrap();
        if let Some(tx) = remote_screen_share.stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        shown.next_wait += 1;
        shown.awaiting_replacement = Some(shown.next_wait);
        shown.next_wait
    };
    log::info!(
        "handle_room_events: live screen track of {sharer_video_identity} unpublished, waiting up to {}s for its replacement",
        SCREEN_SHARE_REPLACEMENT_GRACE.as_secs()
    );
    let remote_screen_share = remote_screen_share.clone();
    let participants = participants.clone();
    let sharer_video_identity = sharer_video_identity.to_string();
    let snapshot_sender = snapshot_sender.clone();
    let event_loop_proxy = event_loop_proxy.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SCREEN_SHARE_REPLACEMENT_GRACE).await;
        // Checked and ended under one lock: a replacement arriving now is handled before (no
        // longer awaited) or after (a new share, in a new window).
        let mut shown = remote_screen_share.shown.lock().unwrap();
        if shown.awaiting_replacement != Some(wait) {
            return;
        }
        log::warn!(
            "handle_room_events: no replacement for the screen track of {sharer_video_identity}, ending the share"
        );
        end_shown_remote_screen_share(
            &mut shown,
            &remote_screen_share,
            &participants,
            &sharer_video_identity,
            &snapshot_sender,
            &event_loop_proxy,
        );
    });
}

struct RoomEventContext {
    receiver: mpsc::UnboundedReceiver<RoomEvent>,
    event_loop_proxy: EventLoopProxy<UserEvent>,
    user_identity: String,
    video_participant_identity: String,
    participants: Arc<std::sync::RwLock<HashMap<String, ParticipantInfo>>>,
    snapshot_sender: SnapshotSender,
    mixer: audio::mixer::MixerHandle,
    remote_screen_share: RemoteScreenShare,
    connection_quality: Arc<std::sync::Mutex<Option<ConnectionQuality>>>,
    audio_handle: TokioHandle,
    inner: Arc<RoomServiceInner>,
    service_command_tx: mpsc::UnboundedSender<RoomServiceCommand>,
}

/// The screen share codec choice for this process: HEVC allowed on an M3 or newer, the setting
/// on until core is told otherwise, and the dev override applied.
fn new_screen_share_codec_choice() -> ScreenShareCodecChoice {
    let mut choice = ScreenShareCodecChoice::new(this_mac_may_share_hevc(), true);
    if let Some(codec) = screen_share_codec_override() {
        choice.force(codec);
    }
    choice
}

/// `HOPP_SCREEN_SHARE_CODEC=h264` or `h265` forces the screen share's codec whatever the
/// watchers, the chip and the setting, for comparing codecs with
/// `core/tests/profile.sh sharer --codec`. A dev override only.
fn screen_share_codec_override() -> Option<ScreenShareCodec> {
    let requested = std::env::var(SCREEN_SHARE_CODEC_ENV).ok()?;
    let codec = match requested.as_str() {
        "h264" => ScreenShareCodec::H264,
        "h265" => ScreenShareCodec::H265,
        _ => {
            log::warn!("screen_share_codec_override: ignoring {SCREEN_SHARE_CODEC_ENV}={requested:?}, expected h264 or h265");
            return None;
        }
    };
    log::warn!(
        "screen_share_codec_override: {SCREEN_SHARE_CODEC_ENV}={requested}: every screen share is {codec:?}, whoever watches (dev override)"
    );
    Some(codec)
}

/// LiveKit's codec for `codec`. Windows shares AV1 where macOS shares H.264, and never picks
/// HEVC (`this_mac_may_share_hevc` is false there).
fn livekit_video_codec(codec: ScreenShareCodec) -> VideoCodec {
    match codec {
        ScreenShareCodec::H264 if SCREEN_SHARE_USES_AV1 => VideoCodec::AV1,
        ScreenShareCodec::H264 => VideoCodec::H264,
        ScreenShareCodec::H265 => VideoCodec::H265,
    }
}

/// Whether a participant on our video room can watch our screen share: another user's main
/// participant or a browser viewer. Never a `:video` participant (they don't subscribe to
/// shares), nor our own main participant, which shows up there as remote and announces our own
/// setting.
fn is_screen_share_watcher(identity: &str, own_identity: &str) -> bool {
    !identity.ends_with(":video") && !participant_identities_match(identity, own_identity)
}

/// Announces with our main participant's attribute whether we can watch HEVC shares.
async fn announce_hevc_support(local_participant: LocalParticipant, supported: bool) {
    let started = Instant::now();
    let announcement = local_participant.set_attributes(screen_share_hevc::announcement(supported));
    match tokio::time::timeout(HEVC_ANNOUNCE_TIMEOUT, announcement).await {
        Ok(Ok(())) => log::info!(
            "announce_hevc_support: {}={} in {}ms",
            screen_share_hevc::ATTRIBUTE,
            screen_share_hevc::attribute_value(supported),
            started.elapsed().as_millis()
        ),
        Ok(Err(e)) => log::error!("announce_hevc_support: failed: {e:?}"),
        Err(_) => log::warn!(
            "announce_hevc_support: no answer from the server in {}ms",
            HEVC_ANNOUNCE_TIMEOUT.as_millis()
        ),
    }
}

/// Publishes a screen track from `source` with `codec`.
async fn publish_screen_share_track(
    local_participant: &LocalParticipant,
    source: &NativeVideoSource,
    codec: ScreenShareCodec,
    muted: bool,
) -> livekit::RoomResult<LocalVideoTrack> {
    let track = LocalVideoTrack::create_video_track(
        VIDEO_TRACK_NAME,
        RtcVideoSource::Native(source.clone()),
    );
    if muted {
        track.mute();
    }
    local_participant
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: livekit_video_codec(codec),
                video_encoding: Some(VideoEncoding {
                    max_bitrate: default_screen_bitrate(),
                    max_framerate: MAX_FRAMERATE,
                }),
                simulcast: false,
                ..Default::default()
            },
        )
        .await?;
    Ok(track)
}

/// The video room's local participant, or `None` outside a call (or when it failed to connect).
async fn video_room_local_participant(inner: &RoomServiceInner) -> Option<LocalParticipant> {
    inner
        .video_room
        .lock()
        .await
        .as_ref()
        .map(Room::local_participant)
}

/// Publishes our screen track again, unmuted, with `codec` (a published track can't change
/// codec), from the same source, so the capture carries on unchanged. Runs when a share starts
/// or during one. A publish that fails is tried once more; if that fails too, the share ends
/// (`screen_share_track_lost`). A hang-up or a new call cancels it.
///
/// The old track goes first. A viewer on an older Hopp closes its window when the track it
/// shows is unsubscribed, and opens it again for the new track; publishing the new one first,
/// it would close it for good. Viewers on this version keep their window open either way
/// (`start_remote_screen_share_stream`).
async fn republish_screen_share_track(
    inner: &RoomServiceInner,
    codec: ScreenShareCodec,
    reason: &str,
    service_command_tx: &mpsc::UnboundedSender<RoomServiceCommand>,
) {
    let republish = republish_screen_share_track_once(inner, codec, reason, service_command_tx);
    if unless_superseded(inner, republish).await.is_none() {
        log::info!("republish_screen_share_track: {reason}: cancelled, the call ended");
        inner.screen_share_codec.lock().unwrap().track_lost();
    }
}

async fn republish_screen_share_track_once(
    inner: &RoomServiceInner,
    codec: ScreenShareCodec,
    reason: &str,
    service_command_tx: &mpsc::UnboundedSender<RoomServiceCommand>,
) {
    let started = Instant::now();
    let local_participant = video_room_local_participant(inner).await;
    let source = inner.buffer_source.lock().unwrap().clone();
    let (Some(local_participant), Some(source)) = (local_participant, source) else {
        log::warn!("republish_screen_share_track: {reason}: no video room or screen source");
        screen_share_track_lost(inner);
        return;
    };
    let old_track = inner.screen_share_track.lock().unwrap().take();
    let old_sid = old_track.as_ref().map(LocalVideoTrack::sid);
    if let Some(old_sid) = &old_sid {
        if let Err(e) = local_participant.unpublish_track(old_sid).await {
            log::warn!("republish_screen_share_track: unpublishing {old_sid} failed: {e:?}");
        }
    }
    let mut attempt = 1;
    let track = loop {
        match publish_screen_share_track(&local_participant, &source, codec, false).await {
            Ok(track) => break track,
            Err(e) if attempt < SCREEN_SHARE_PUBLISH_ATTEMPTS => {
                log::warn!(
                    "republish_screen_share_track: {reason}: publishing the {codec:?} track failed after {}ms, trying again: {e:?}",
                    started.elapsed().as_millis()
                );
                tokio::time::sleep(SCREEN_SHARE_PUBLISH_RETRY_DELAY).await;
                attempt += 1;
            }
            Err(e) => {
                log::error!(
                    "republish_screen_share_track: {reason}: publishing the {codec:?} track failed {attempt} times in {}ms: {e:?}",
                    started.elapsed().as_millis()
                );
                screen_share_track_lost(inner);
                return;
            }
        }
    };
    log::info!(
        "republish_screen_share_track: {reason}: {codec:?} track {} replaced {old_sid:?} in {}ms",
        track.sid(),
        started.elapsed().as_millis()
    );
    *inner.screen_share_track.lock().unwrap() = Some(track.clone());
    // A new track starts with the publish defaults: low-bandwidth mode needs its encoding again
    // (spec 0007 B5).
    apply_screen_share_encoding(inner);
    spawn_screen_share_encoder_check(
        inner,
        track,
        codec,
        SCREEN_SHARE_TRACK_WARMUP,
        service_command_tx,
    );
}

/// No screen track is published: a republish failed. A share in progress can't go on without
/// one, so it ends the way any share does, which the sharer's call controls show; the next
/// share publishes a new track.
fn screen_share_track_lost(inner: &RoomServiceInner) {
    let sharing = {
        let mut codec_choice = inner.screen_share_codec.lock().unwrap();
        codec_choice.track_lost();
        codec_choice.sharing()
    };
    if !sharing {
        return;
    }
    log::error!("screen_share_track_lost: ending the share, it has no screen track");
    if let Err(e) = inner
        .event_loop_proxy
        .send_event(UserEvent::StopScreenShare)
    {
        log::error!("screen_share_track_lost: failed to send StopScreenShare: {e:?}");
    }
}

/// Runs `work` unless the call is superseded first (`ConnectGate::superseded`): the command
/// loop then moves on to the hang-up or new call queued behind it. `None` when cancelled.
async fn unless_superseded<T>(
    inner: &RoomServiceInner,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    let superseded = inner.connect_gate.lock().unwrap().superseded();
    tokio::select! {
        result = work => Some(result),
        _ = superseded => None,
    }
}

/// Republishes the screen track when the codec choice asked for it.
async fn republish_if_needed(
    inner: &RoomServiceInner,
    republish: Option<ScreenShareCodec>,
    reason: &str,
    service_command_tx: &mpsc::UnboundedSender<RoomServiceCommand>,
) {
    if let Some(codec) = republish {
        republish_screen_share_track(inner, codec, reason, service_command_tx).await;
    }
}

/// Checks in the background that `track`, just published or unmuted for a share, encodes
/// `codec`. A failed HEVC check makes the share fall back to H.264 (spec 0007 B6). A new track
/// gets its last frame resent for `warmup` (see `check_screen_share_encoder`).
fn spawn_screen_share_encoder_check(
    inner: &RoomServiceInner,
    track: LocalVideoTrack,
    codec: ScreenShareCodec,
    warmup: Duration,
    service_command_tx: &mpsc::UnboundedSender<RoomServiceCommand>,
) {
    let event_loop_proxy = inner.event_loop_proxy.clone();
    let service_command_tx = service_command_tx.clone();
    tokio::spawn(async move {
        let Err(reason) =
            check_screen_share_encoder(&track, codec, warmup, &event_loop_proxy).await
        else {
            return;
        };
        if codec == ScreenShareCodec::H265 {
            let _ = service_command_tx.send(RoomServiceCommand::HevcEncodingFailed {
                track_sid: track.sid(),
                reason,
            });
        } else {
            log::warn!(
                "check_screen_share_encoder: {codec:?} track {}: {reason}",
                track.sid()
            );
        }
    });
}

/// Waits until `track` has encoded a frame as `codec` (`EncoderCheck` has the rules). Ends
/// quietly once the track is muted or unpublished, or when the capture submits nothing.
///
/// ScreenCaptureKit sends no frames while the screen is static, so a track could wait for its
/// first frame until something moves. Until a new frame is encoded, and on a new track for
/// `warmup` after the check started, the capture is asked to resend its last frame every
/// `SCREEN_SHARE_ENCODER_CHECK_INTERVAL`: a viewer subscribing to a new track gets its first
/// frame from the next key frame, which needs a frame to encode.
///
/// For HEVC it also fails unless WebRTC logged the encoder's compression session as hardware
/// accelerated (`webrtc_log`): VideoToolbox may use its software encoder, and the stats report
/// `VideoToolbox` either way. Besides that, it catches an HEVC encoder that never starts, and
/// WebRTC switching the track to another codec after the encoder failed to initialize.
async fn check_screen_share_encoder(
    track: &LocalVideoTrack,
    codec: ScreenShareCodec,
    warmup: Duration,
    event_loop_proxy: &EventLoopProxy<UserEvent>,
) -> Result<(), String> {
    let expected = livekit_video_codec(codec).as_str().to_ascii_uppercase();
    let mut check = EncoderCheck::new(codec, expected, warmup);
    let started = Instant::now();
    loop {
        tokio::time::sleep(SCREEN_SHARE_ENCODER_CHECK_INTERVAL).await;
        if track.is_muted() {
            return Ok(());
        }
        let Ok(progress) = crate::livekit::stats::encoder_progress(track).await else {
            return Ok(());
        };
        let session = crate::webrtc_log::last_hevc_session();
        let was_encoding = check.encoding();
        let step = check.observe(progress.as_ref(), session, started.elapsed());
        if let Some(progress) = progress.filter(|_| !was_encoding && check.encoding()) {
            log::info!(
                "check_screen_share_encoder: track {} encoding {} with {} after {}ms",
                track.sid(),
                progress.codec,
                progress.implementation,
                started.elapsed().as_millis()
            );
        }
        match step {
            EncoderCheckStep::Continue => {
                let _ = event_loop_proxy.send_event(UserEvent::ResendScreenShareFrame);
            }
            EncoderCheckStep::Done => return Ok(()),
            EncoderCheckStep::Inconclusive => {
                log::warn!(
                    "check_screen_share_encoder: the capture submitted no frame to track {} in {}s; its encoder isn't checked",
                    track.sid(),
                    ENCODER_CHECK_IDLE_LIMIT.as_secs()
                );
                return Ok(());
            }
            EncoderCheckStep::Failed(reason) => return Err(reason),
        }
    }
}

/// Stops the black keepalive frames of a track published muted, once the SFU has seen it.
///
/// `NativeVideoSource::new` (our LiveKit fork, from upstream) captures a black frame every
/// 100 ms until the source's first `capture_frame`, so a published track carries media before
/// it has real frames. Core publishes its camera and screen tracks muted at call start, so a
/// participant who never turned them on encoded and sent those frames for the whole call (about
/// 1% of a core, plus traffic). After `VIDEO_KEEPALIVE_GRACE`, a track still muted gets one
/// frame captured here, which ends the keepalive. A muted track's frames go out black anyway,
/// and a track in use already ended it with its first real frame.
fn end_video_keepalive(source: NativeVideoSource, track: LocalVideoTrack, label: &'static str) {
    tokio::spawn(async move {
        tokio::time::sleep(VIDEO_KEEPALIVE_GRACE).await;
        if !track.is_muted() {
            return;
        }
        let resolution = source.video_resolution();
        let mut buffer = I420Buffer::new(resolution.width, resolution.height);
        let (y, u, v) = buffer.data_mut();
        y.fill(16);
        u.fill(128);
        v.fill(128);
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer,
        });
        log::info!("end_video_keepalive: {label} track still muted, keepalive frames stopped");
    });
}

/// The call room's local participant, or `None` outside a call.
///
/// Returns an owned handle so the `room` lock is released before the caller awaits
/// `publish_data`. That call can wait seconds (a reconnect, the publisher connection, data
/// channel backpressure on a congested call), and meanwhile the room-event loop and the stats
/// loop need the lock. Publishes stay ordered without it: they all run on the command task.
async fn room_local_participant(inner: &RoomServiceInner) -> Option<LocalParticipant> {
    inner
        .room
        .lock()
        .await
        .as_ref()
        .map(Room::local_participant)
}

async fn publish_bandwidth_mode_request(inner: &RoomServiceInner, low_bandwidth: bool) {
    let Some(local_participant) = room_local_participant(inner).await else {
        log::warn!("publish_bandwidth_mode_request: Room doesn't exist");
        return;
    };
    let payload = serde_json::to_vec(&BandwidthModeRequest { low_bandwidth }).unwrap();
    let res = local_participant
        .publish_data(DataPacket {
            payload,
            reliable: true,
            topic: Some(TOPIC_BANDWIDTH_MODE.to_string()),
            ..Default::default()
        })
        .await;
    if let Err(e) = res {
        log::error!("publish_bandwidth_mode_request: Failed to publish: {e:?}");
    }
}

/// Applies the effective bandwidth mode to the screen share encoding and the
/// camera subscriptions, then reports the state to the UI.
async fn apply_bandwidth_mode(inner: &RoomServiceInner) {
    apply_screen_share_encoding(inner);
    update_camera_quality(inner).await;
    send_bandwidth_mode_state(inner);
}

fn apply_screen_share_encoding(inner: &RoomServiceInner) {
    let Some((width, height)) = *inner.capture_size.lock().unwrap() else {
        return;
    };
    let low = inner.bandwidth.lock().unwrap().effective();
    let encoding = screen_encoding(low, width, height);
    let screen_share_track = inner.screen_share_track.lock().unwrap();
    let Some(track) = screen_share_track.as_ref() else {
        return;
    };
    let res = track.set_encoding_parameters(VideoEncodingUpdate {
        max_bitrate: Some(encoding.max_bitrate),
        max_framerate: Some(encoding.max_framerate),
        scale_resolution_down_by: Some(encoding.scale_down_by),
        degradation_preference: Some(DegradationPreference::MaintainResolution),
    });
    if let Err(e) = res {
        // The encoder keeps its previous rate, so the capture keeps following it.
        log::error!("apply_screen_share_encoding: Failed to set encoding: {e:?}");
        return;
    }
    log::info!(
        "apply_screen_share_encoding: low={low} capture={width}x{height} encoding={encoding:?}"
    );
    if let Err(e) = inner
        .event_loop_proxy
        .send_event(UserEvent::ScreenShareEncoderFramerate(
            encoding.max_framerate,
        ))
    {
        log::error!("apply_screen_share_encoding: Failed to send the capture frame rate: {e:?}");
    }
}

fn bandwidth_mode_state(inner: &RoomServiceInner) -> socket_lib::BandwidthModeState {
    let (active, local_requested, requesters) = {
        let bandwidth = inner.bandwidth.lock().unwrap();
        (
            bandwidth.effective(),
            bandwidth.local(),
            bandwidth.requesters(),
        )
    };
    let requested_by = {
        let participants = inner.participants.read().unwrap();
        requesters
            .iter()
            .map(|requester| {
                participants
                    .iter()
                    .find(|(identity, _)| participant_identities_match(identity, requester))
                    .map(|(_, info)| info.name().to_string())
                    .unwrap_or_else(|| requester.clone())
            })
            .collect()
    };
    socket_lib::BandwidthModeState {
        active,
        local_requested,
        requested_by,
    }
}

fn send_bandwidth_mode_state(inner: &RoomServiceInner) {
    let state = bandwidth_mode_state(inner);
    log::info!("send_bandwidth_mode_state: {state:?}");
    if let Err(e) = inner
        .event_loop_proxy
        .send_event(UserEvent::BandwidthModeStateChanged(state.clone()))
    {
        log::error!("send_bandwidth_mode_state: Failed to send to event loop: {e:?}");
    }
    inner.snapshot_sender.send_bandwidth_mode_state(state);
}

fn camera_quality(active: usize) -> VideoQuality {
    match active {
        0..=3 => VideoQuality::High,
        4..=6 => VideoQuality::Medium,
        _ => VideoQuality::Low,
    }
}

async fn update_camera_quality(inner: &RoomServiceInner) {
    let (active, camera_publications) = {
        let room = inner.room.lock().await;
        let Some(room) = room.as_ref() else {
            return;
        };
        let local_camera_active =
            room.local_participant()
                .track_publications()
                .values()
                .any(|publication| {
                    publication.source() == TrackSource::Camera && !publication.is_muted()
                });
        let remote_participants = room.remote_participants();
        let active = usize::from(local_camera_active)
            + remote_participants
                .values()
                .filter(|participant| {
                    participant
                        .track_publications()
                        .values()
                        .any(|publication| {
                            publication.source() == TrackSource::Camera && !publication.is_muted()
                        })
                })
                .count();
        let camera_publications = remote_participants
            .values()
            .flat_map(|participant| participant.track_publications().into_values())
            .filter(|publication| publication.source() == TrackSource::Camera)
            .collect::<Vec<_>>();
        (active, camera_publications)
    };
    let low_bandwidth = inner.bandwidth.lock().unwrap().effective();
    let quality = if low_bandwidth {
        VideoQuality::Low
    } else {
        camera_quality(active)
    };
    let mut updated = 0;
    for publication in camera_publications {
        if publication.simulcasted() {
            publication.set_video_quality(quality);
            updated += 1;
        }
    }
    log::info!(
        "camera quality: active={active}, low_bandwidth={low_bandwidth}, quality={quality:?}, updated_publications={updated}"
    );
}

/// Events of our video room, where our screen track is published: they tell who can watch our
/// share (spec 0007). `generation` is the connect it belongs to, so a late event from an old
/// call's room changes nothing.
async fn handle_video_room_events(
    mut receiver: mpsc::UnboundedReceiver<RoomEvent>,
    own_identity: String,
    service_command_tx: mpsc::UnboundedSender<RoomServiceCommand>,
    generation: u64,
) {
    let send = |event: ScreenShareWatcherEvent| {
        let _ = service_command_tx.send(RoomServiceCommand::ScreenShareWatcher(event, generation));
    };
    while let Some(event) = receiver.recv().await {
        match event {
            RoomEvent::ParticipantConnected(participant)
                if is_screen_share_watcher(participant.identity().as_str(), &own_identity) =>
            {
                send(ScreenShareWatcherEvent::Seen {
                    identity: participant.identity().to_string(),
                    announced: screen_share_hevc::announced(&participant.attributes()),
                });
            }
            RoomEvent::ParticipantAttributesChanged {
                participant: Participant::Remote(participant),
                changed_attributes,
            } if changed_attributes.contains_key(screen_share_hevc::ATTRIBUTE)
                && is_screen_share_watcher(participant.identity().as_str(), &own_identity) =>
            {
                send(ScreenShareWatcherEvent::Seen {
                    identity: participant.identity().to_string(),
                    announced: screen_share_hevc::announced(&participant.attributes()),
                });
            }
            RoomEvent::TrackPublished { participant, .. }
                if is_screen_share_watcher(participant.identity().as_str(), &own_identity)
                    && screen_share_hevc::announced(&participant.attributes()).is_none() =>
            {
                send(ScreenShareWatcherEvent::PublishedUnannounced {
                    identity: participant.identity().to_string(),
                });
            }
            RoomEvent::ParticipantDisconnected(participant)
                if is_screen_share_watcher(participant.identity().as_str(), &own_identity) =>
            {
                send(ScreenShareWatcherEvent::Left {
                    identity: participant.identity().to_string(),
                });
            }
            RoomEvent::Reconnecting => {
                log::warn!("handle_video_room_events: video room connection lost, reconnecting")
            }
            RoomEvent::Reconnected => {
                log::info!("handle_video_room_events: video room reconnected")
            }
            RoomEvent::Disconnected { reason } => {
                log::warn!("handle_video_room_events: video room disconnected: {reason:?}")
            }
            _ => {}
        }
    }
    log::info!("handle_video_room_events: video_room event channel closed");
}

async fn handle_room_events(ctx: RoomEventContext) {
    let RoomEventContext {
        mut receiver,
        event_loop_proxy,
        user_identity,
        video_participant_identity,
        participants,
        snapshot_sender,
        mixer,
        remote_screen_share,
        connection_quality,
        audio_handle,
        inner,
        service_command_tx,
    } = ctx;
    while let Some(msg) = receiver.recv().await {
        match msg {
            RoomEvent::DataReceived {
                payload,
                topic,
                kind: _,
                participant,
            } => {
                if topic.as_deref() == Some(TOPIC_APP_VEIL) {
                    let Some(participant) = participant else {
                        log::warn!("handle_room_events: App Veil sender is missing");
                        continue;
                    };
                    let sender = participant.identity().as_str().to_string();
                    if sender == user_identity {
                        continue;
                    }
                    let snapshot = match serde_json::from_slice::<AppVeilSnapshot>(&payload) {
                        Ok(snapshot) => sanitize_app_veil_snapshot(snapshot),
                        Err(error) => {
                            log::warn!("handle_room_events: Invalid App Veil snapshot: {error:?}");
                            continue;
                        }
                    };
                    let publisher = remote_screen_share
                        .publisher_identity
                        .lock()
                        .unwrap()
                        .clone();
                    if publisher
                        .as_deref()
                        .is_some_and(|publisher| !participant_identities_match(publisher, &sender))
                    {
                        log::debug!(
                            "handle_room_events: Ignoring App Veil snapshot from non-sharer {sender}"
                        );
                        continue;
                    }
                    *remote_screen_share.app_veil_snapshot.lock().unwrap() =
                        Some((sender, snapshot.clone()));
                    if publisher.is_some() {
                        if let Err(error) =
                            event_loop_proxy.send_event(UserEvent::AppVeilSnapshot(snapshot))
                        {
                            log::error!(
                                "handle_room_events: Failed to send AppVeilSnapshot: {error:?}"
                            );
                        }
                    }
                    continue;
                }

                if topic.as_deref() == Some(TOPIC_BANDWIDTH_MODE) {
                    let Some(participant) = participant else {
                        log::warn!("handle_room_events: Bandwidth mode sender is missing");
                        continue;
                    };
                    let sender = participant.identity().as_str().to_string();
                    if sender == user_identity {
                        continue;
                    }
                    let request = match serde_json::from_slice::<BandwidthModeRequest>(&payload) {
                        Ok(request) => request,
                        Err(error) => {
                            log::warn!(
                                "handle_room_events: Invalid bandwidth mode request: {error:?}"
                            );
                            continue;
                        }
                    };
                    log::info!(
                        "handle_room_events: Bandwidth mode request from {sender}: {}",
                        request.low_bandwidth
                    );
                    let (effective_changed, requesters_changed) = {
                        let mut bandwidth = inner.bandwidth.lock().unwrap();
                        let requesters_before = bandwidth.requesters();
                        let effective_changed =
                            bandwidth.set_remote(&sender, request.low_bandwidth);
                        (
                            effective_changed,
                            requesters_before != bandwidth.requesters(),
                        )
                    };
                    if effective_changed {
                        apply_bandwidth_mode(&inner).await;
                    } else if requesters_changed {
                        send_bandwidth_mode_state(&inner);
                    }
                    continue;
                }

                if topic.as_deref() == Some(TOPIC_EFFECT) {
                    let Some(participant) = participant else {
                        log::debug!("handle_room_events: effect sender is missing");
                        continue;
                    };
                    let sender = participant.identity().as_str().to_string();
                    if participant_identities_match(&sender, &user_identity) {
                        continue;
                    }
                    // Size, version, id and point are validated; unknown ids dropped.
                    let Some(trigger) = parse_effect_packet(&payload) else {
                        log::debug!("handle_room_events: dropping invalid effect from {sender}");
                        continue;
                    };
                    if let Err(e) = event_loop_proxy
                        .send_event(UserEvent::EffectFromParticipant(trigger.effect, sender))
                    {
                        log::error!(
                            "handle_room_events: Failed to send EffectFromParticipant: {e:?}"
                        );
                    }
                    continue;
                }

                // participant_in_control uses raw UTF-8 identity, not JSON. Handle before deserialize.
                // TODO(@konsalex): Maybe follow a JSON  type
                // type, payload approach to be easier to work with?
                if topic.as_deref() == Some(TOPIC_PARTICIPANT_IN_CONTROL) {
                    if let Ok(identity_str) = std::str::from_utf8(&payload) {
                        let in_control = identity_str == user_identity;
                        if let Err(e) = event_loop_proxy
                            .send_event(UserEvent::LocalParticipantInControl(in_control))
                        {
                            log::error!("handle_room_events: Failed to send LocalParticipantInControl: {e:?}");
                        }
                    } else {
                        log::warn!(
                            "handle_room_events: participant_in_control payload is not valid UTF-8"
                        );
                    }

                    continue;
                }

                let client_event: ClientEvent = match serde_json::from_slice(&payload) {
                    Ok(event) => event,
                    Err(e) => {
                        log::error!("handle_room_events: Failed to deserialize event: {e:?}");
                        continue;
                    }
                };
                log::debug!("handle_room_events: Data received: {client_event:?}");
                let identity = if let Some(participant) = participant {
                    participant.identity().as_str().to_string()
                } else {
                    log::warn!("handle_room_events: Participant is none");
                    "".to_string()
                };

                /* Skip our own events. */
                if identity == user_identity {
                    log::debug!("handle_room_events: Skipping own event");
                    continue;
                }

                let res = match client_event {
                    ClientEvent::MouseMove(point) => {
                        /* let point = translate_mouse_position(point, menu_perc); */
                        event_loop_proxy.send_event(UserEvent::CursorPosition(
                            point.x as f32,
                            point.y as f32,
                            identity,
                        ))
                    }
                    ClientEvent::MouseClick(click) => {
                        event_loop_proxy.send_event(UserEvent::MouseClick(
                            crate::MouseClickData {
                                x: click.x as f32,
                                y: click.y as f32,
                                button: click.button,
                                clicks: click.clicks as f32,
                                down: click.down,
                                shift: click.shift,
                                meta: click.meta,
                                ctrl: click.ctrl,
                                alt: click.alt,
                            },
                            identity,
                        ))
                    }
                    ClientEvent::Keystroke(key) => {
                        event_loop_proxy.send_event(UserEvent::Keystroke(crate::KeystrokeData {
                            key: key.key[0].clone(),
                            meta: key.meta,
                            ctrl: key.ctrl,
                            shift: key.shift,
                            alt: key.alt,
                            down: key.down,
                        }))
                    }
                    ClientEvent::WheelEvent(wheel_data) => {
                        event_loop_proxy.send_event(UserEvent::Scroll(
                            crate::ScrollDelta {
                                x: wheel_data.deltaX,
                                y: wheel_data.deltaY,
                            },
                            identity,
                        ))
                    }
                    ClientEvent::Tick(tick_data) => {
                        if cfg!(debug_assertions) {
                            event_loop_proxy.send_event(UserEvent::Tick(tick_data.time))
                        } else {
                            Ok(())
                        }
                    }
                    ClientEvent::AddToClipboard(add_to_clipboard_data) => event_loop_proxy
                        .send_event(UserEvent::AddToClipboard(add_to_clipboard_data, identity)),
                    ClientEvent::PasteFromClipboard(paste_from_clipboard_data) => event_loop_proxy
                        .send_event(UserEvent::PasteFromClipboard(paste_from_clipboard_data)),
                    ClientEvent::DrawingMode(drawing_mode) => {
                        event_loop_proxy.send_event(UserEvent::DrawingMode(drawing_mode, identity))
                    }
                    ClientEvent::DrawStart(draw_path_point) => {
                        event_loop_proxy.send_event(UserEvent::DrawStart(
                            draw_path_point.point,
                            draw_path_point.path_id,
                            identity,
                        ))
                    }
                    ClientEvent::DrawAddPoint(point) => {
                        event_loop_proxy.send_event(UserEvent::DrawAddPoint(point, identity))
                    }
                    ClientEvent::DrawEnd(point) => {
                        event_loop_proxy.send_event(UserEvent::DrawEnd(point, identity))
                    }
                    ClientEvent::DrawClearPath { path_id } => {
                        event_loop_proxy.send_event(UserEvent::DrawClearPath(path_id, identity))
                    }
                    ClientEvent::DrawClearAllPaths => {
                        event_loop_proxy.send_event(UserEvent::DrawClearAllPaths(identity))
                    }
                    ClientEvent::DrawText(data) => {
                        event_loop_proxy.send_event(UserEvent::DrawText(data, identity))
                    }
                    ClientEvent::ClickAnimation(point) => event_loop_proxy
                        .send_event(UserEvent::ClickAnimationFromParticipant(point, identity)),
                    ClientEvent::RemoteControlEnabled(data) => {
                        event_loop_proxy.send_event(UserEvent::SharerControlEnabled(data.enabled))
                    }
                    ClientEvent::ClipboardData(payload) => {
                        if payload.requester_sid == user_identity {
                            event_loop_proxy.send_event(UserEvent::SetClipboard(payload))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Ok(()),
                };
                if let Err(e) = res {
                    log::error!("handle_room_events: Failed to send message: {e:?}");
                }
            }
            RoomEvent::ParticipantConnected(participant) => {
                let identity = participant.identity().as_str().to_string();
                let name = participant.name();

                log::info!("handle_room_events: Participant connected: {}", identity);

                if !insert_participant_if_absent(&participants, &identity, &participant) {
                    continue;
                }
                if let Err(e) =
                    event_loop_proxy.send_event(UserEvent::ParticipantConnected(ParticipantData {
                        name,
                        identity: identity.clone(),
                    }))
                {
                    log::error!(
                        "handle_room_events: Failed to send participant connected event: {e:?}"
                    );
                }

                // Late joiners need our low-bandwidth request, nothing else replays it.
                let _ = service_command_tx.send(RoomServiceCommand::RepublishBandwidthModeRequest);

                snapshot_sender.send_participants_snapshot();
            }
            RoomEvent::ParticipantActive(_) => {
                let _ = service_command_tx
                    .send(RoomServiceCommand::PublishAppVeilSnapshot { force: true });
            }
            RoomEvent::ParticipantDisconnected(participant) => {
                let identity = participant.identity().as_str().to_string();
                let name = participant.name();

                log::info!("handle_room_events: Participant disconnected: {}", identity);

                let disconnected_sharer = remote_screen_share
                    .publisher_identity
                    .lock()
                    .unwrap()
                    .clone()
                    .filter(|publisher| participant_identities_match(publisher, &identity));
                if let Some(sharer_video_identity) = disconnected_sharer {
                    end_remote_screen_share(
                        &remote_screen_share,
                        &participants,
                        &sharer_video_identity,
                        &snapshot_sender,
                        &event_loop_proxy,
                    );
                }

                // Stop streams and remove from HashMap
                let any_camera_active_after = {
                    let mut participants_guard = participants.write().unwrap();
                    if let Some(info) = participants_guard.get_mut(&identity) {
                        info.stop_audio_stream();
                        info.stop_camera_stream();
                    }
                    participants_guard.remove(&identity);
                    participants_guard.values().any(|info| info.camera_active())
                };

                if !any_camera_active_after {
                    if let Err(e) = event_loop_proxy.send_event(UserEvent::CloseCameraWindow) {
                        log::error!(
                            "handle_room_events: Failed to send CloseCameraWindow event: {e:?}"
                        );
                    }
                }
                let (bandwidth_changed, requesters_changed) = {
                    let mut bandwidth = inner.bandwidth.lock().unwrap();
                    let requesters_before = bandwidth.requesters();
                    // A request is keyed by the user, so only their main identity leaving withdraws it.
                    let effective_changed =
                        !identity.ends_with(":video") && bandwidth.remove(&identity);
                    (
                        effective_changed,
                        requesters_before != bandwidth.requesters(),
                    )
                };
                if bandwidth_changed {
                    apply_bandwidth_mode(&inner).await;
                } else {
                    update_camera_quality(&inner).await;
                    if requesters_changed {
                        send_bandwidth_mode_state(&inner);
                    }
                }

                if let Err(e) = event_loop_proxy.send_event(UserEvent::ParticipantDisconnected(
                    ParticipantData { name, identity },
                )) {
                    log::error!(
                        "handle_room_events: Failed to send participant disconnected event: {e:?}"
                    );
                }

                snapshot_sender.send_participants_snapshot();
            }
            RoomEvent::TrackPublished {
                publication,
                participant,
            } => {
                if participant.identity().as_str() == video_participant_identity {
                    publication.set_subscribed(false);
                    continue;
                }
                log::info!(
                    "handle_room_events: Track published: {} ({:?}) from {}",
                    publication.name(),
                    publication.source(),
                    participant.identity()
                );
                if publication.source() == TrackSource::Camera {
                    update_camera_quality(&inner).await;
                }
            }
            RoomEvent::ActiveSpeakersChanged { speakers } => {
                log::trace!("handle_room_events: Active speakers changed");
                let mut participants_guard = participants.write().unwrap();

                // First, set all participants to not speaking
                for info in participants_guard.values_mut() {
                    info.set_is_speaking(false);
                }

                // Then set active speakers to speaking
                for speaker in speakers {
                    let identity = speaker.identity().as_str().to_string();
                    if identity == user_identity {
                        if let Some(info) = participants_guard.get_mut("local") {
                            info.set_is_speaking(true);
                        }
                    } else if let Some(info) = participants_guard.get_mut(&identity) {
                        info.set_is_speaking(true);
                    }
                }
            }
            RoomEvent::TrackMuted {
                participant,
                publication,
            } => {
                let identity = participant.identity().as_str().to_string();
                if identity == user_identity || identity == video_participant_identity {
                    log::info!(
                        "handle_room_event: skip same user's muted event, type {:?}",
                        publication.source()
                    );
                    continue;
                }

                match (publication.kind(), publication.source()) {
                    (livekit::track::TrackKind::Audio, _) => {
                        log::info!("handle_room_events: Audio track muted for {}", identity);
                        {
                            let mut participants_guard = participants.write().unwrap();
                            if let Some(info) = participants_guard.get_mut(&identity) {
                                info.set_muted(true);
                            }
                        }
                        snapshot_sender.send_participants_snapshot();
                    }
                    (livekit::track::TrackKind::Video, TrackSource::Camera) => {
                        log::info!("handle_room_events: Camera track muted for {}", identity);
                        let any_camera_active = {
                            let mut guard = participants.write().unwrap();
                            if let Some(info) = guard.get_mut(&identity) {
                                info.stop_camera_stream();
                            }
                            guard.values().any(|info| info.camera_active())
                        };
                        if !any_camera_active {
                            if let Err(e) =
                                event_loop_proxy.send_event(UserEvent::CloseCameraWindow)
                            {
                                log::error!(
                                    "handle_room_events: Failed to send CloseCameraWindow event: {e:?}"
                                );
                            }
                        }
                        update_camera_quality(&inner).await;
                    }
                    (livekit::track::TrackKind::Video, TrackSource::Screenshare) => {
                        log::info!("handle_room_events: Screen share muted from {}", identity);

                        // Only clean up if this is the track we show
                        if !is_shown_screen_share(
                            &remote_screen_share,
                            &identity,
                            &publication.sid(),
                        ) {
                            log::info!(
                                "handle_room_events: Ignoring mute from non-current publisher {}",
                                identity,
                            );
                            continue;
                        }

                        end_remote_screen_share(
                            &remote_screen_share,
                            &participants,
                            &identity,
                            &snapshot_sender,
                            &event_loop_proxy,
                        );
                    }
                    _ => {}
                }
            }
            RoomEvent::TrackUnmuted {
                participant,
                publication,
            } => {
                let identity = participant.identity().as_str().to_string();
                if identity == user_identity || identity == video_participant_identity {
                    log::info!(
                        "handle_room_event: skip same user's unmuted event, type {:?}",
                        publication.source()
                    );
                    continue;
                }

                match (publication.kind(), publication.source()) {
                    (livekit::track::TrackKind::Audio, _) => {
                        log::info!("handle_room_events: Audio track unmuted for {}", identity);
                        {
                            let mut participants_guard = participants.write().unwrap();
                            if let Some(info) = participants_guard.get_mut(&identity) {
                                info.set_muted(false);
                            }
                        }
                        snapshot_sender.send_participants_snapshot();
                    }
                    (livekit::track::TrackKind::Video, TrackSource::Camera) => {
                        log::info!("handle_room_events: Camera track unmuted for {}", identity);
                        let video_track = match publication.track() {
                            Some(livekit::track::Track::RemoteVideo(vt)) => vt,
                            _ => {
                                log::warn!(
                                    "handle_room_events: No remote video track in publication"
                                );
                                continue;
                            }
                        };
                        start_remote_camera_stream(
                            video_track,
                            &participants,
                            &identity,
                            &event_loop_proxy,
                        );
                        update_camera_quality(&inner).await;
                    }
                    (livekit::track::TrackKind::Video, TrackSource::Screenshare) => {
                        log::info!(
                            "handle_room_events: Screen share track unmuted for {}",
                            identity
                        );
                        let video_track = match publication.track() {
                            Some(livekit::track::Track::RemoteVideo(vt)) => vt,
                            _ => {
                                log::warn!(
                                    "handle_room_events: No remote video track in publication"
                                );
                                continue;
                            }
                        };
                        start_remote_screen_share_stream(
                            video_track,
                            publication.sid(),
                            &remote_screen_share,
                            &participants,
                            &identity,
                            &event_loop_proxy,
                            &snapshot_sender,
                        );
                    }
                    _ => {}
                }
            }
            RoomEvent::TrackSubscribed {
                track,
                publication,
                participant,
            } => {
                log::info!(
                    "handle_room_events: Track subscribed from {}: {} ({:?}) {:?} {:?}",
                    participant.identity(),
                    track.name(),
                    track.kind(),
                    track,
                    publication,
                );

                let participant_identity = participant.identity().as_str().to_string();

                if participant_identity == video_participant_identity {
                    publication.set_subscribed(false);
                    log::debug!("handle_room_events: Unsubscribed from video participant track");
                    continue;
                }

                if insert_participant_if_absent(&participants, &participant_identity, &participant)
                {
                    log::info!(
                        "handle_room_events: Creating participant {} from track subscription",
                        participant_identity
                    );
                }
                match track {
                    livekit::track::RemoteTrack::Audio(audio_track) => {
                        log::info!(
                            "handle_room_events: Setting up audio stream for participant: {}",
                            participant_identity
                        );

                        let handle = crate::livekit::audio::play_remote_audio_track(
                            audio_track,
                            mixer.clone(),
                            &participant_identity,
                            &audio_handle,
                        );

                        let mut participants_guard = participants.write().unwrap();
                        if let Some(info) = participants_guard.get_mut(&participant_identity) {
                            info.set_audio_handle(handle);
                        }
                    }
                    livekit::track::RemoteTrack::Video(video_track) => match publication.source() {
                        TrackSource::Screenshare => {
                            if !publication.is_muted() {
                                log::info!(
                                    "handle_room_events: Screen share track subscribed and already unmuted for {}",
                                    participant_identity
                                );
                                start_remote_screen_share_stream(
                                    video_track,
                                    publication.sid(),
                                    &remote_screen_share,
                                    &participants,
                                    &participant_identity,
                                    &event_loop_proxy,
                                    &snapshot_sender,
                                );
                            } else {
                                log::info!(
                                    "handle_room_events: Screen share track subscribed (muted) for {}",
                                    participant_identity
                                );
                            }
                        }
                        TrackSource::Camera => {
                            update_camera_quality(&inner).await;
                            if !publication.is_muted() {
                                log::info!(
                                    "handle_room_events: Camera track subscribed and already unmuted for {}",
                                    participant_identity
                                );
                                start_remote_camera_stream(
                                    video_track,
                                    &participants,
                                    &participant_identity,
                                    &event_loop_proxy,
                                );
                            } else {
                                log::info!(
                                    "handle_room_events: Camera track subscribed (muted) for {}",
                                    participant_identity
                                );
                            }
                        }
                        source => {
                            log::info!(
                                "handle_room_events: Ignoring non-camera video track: {} ({:?})",
                                video_track.name(),
                                source
                            );
                        }
                    },
                }
            }
            RoomEvent::TrackUnsubscribed {
                track,
                publication,
                participant,
            } => {
                log::info!(
                    "handle_room_events: Track unsubscribed from {}: {} ({:?})",
                    participant.identity(),
                    track.name(),
                    track.kind()
                );

                let participant_identity = participant.identity().as_str().to_string();

                if participant_identity == video_participant_identity {
                    log::debug!("handle_room_events: Skipping track unsubscribed event from video participant");
                    continue;
                }

                match track {
                    livekit::track::RemoteTrack::Video(_) => {
                        match publication.source() {
                            TrackSource::Camera => {
                                log::info!(
                                    "handle_room_events: Camera track unsubscribed from {}",
                                    participant_identity
                                );
                                let any_camera_active = {
                                    let mut guard = participants.write().unwrap();
                                    if let Some(info) = guard.get_mut(&participant_identity) {
                                        info.stop_camera_stream();
                                    }
                                    guard.values().any(|info| info.camera_active())
                                };
                                if !any_camera_active {
                                    if let Err(e) =
                                        event_loop_proxy.send_event(UserEvent::CloseCameraWindow)
                                    {
                                        log::error!(
                                            "handle_room_events: Failed to send CloseCameraWindow event: {e:?}"
                                        );
                                    }
                                }
                            }
                            TrackSource::Screenshare => {
                                log::info!(
                                    "handle_room_events: Screen share track unsubscribed from {}",
                                    participant_identity
                                );

                                if !is_shown_screen_share(
                                    &remote_screen_share,
                                    &participant_identity,
                                    &publication.sid(),
                                ) {
                                    continue;
                                }
                                // A live track's unpublish from a Hopp sharer (its `:video`
                                // participant) is a codec switch; its share ends with a mute.
                                if !publication.is_muted()
                                    && participant_identity.ends_with(":video")
                                {
                                    await_screen_share_replacement(
                                        &remote_screen_share,
                                        &participants,
                                        &participant_identity,
                                        &snapshot_sender,
                                        &event_loop_proxy,
                                    );
                                } else {
                                    end_remote_screen_share(
                                        &remote_screen_share,
                                        &participants,
                                        &participant_identity,
                                        &snapshot_sender,
                                        &event_loop_proxy,
                                    );
                                }
                            }
                            source => {
                                log::info!(
                                    "handle_room_events: Video track unsubscribed from {} ({:?})",
                                    participant_identity,
                                    source,
                                );
                            }
                        }
                    }
                    livekit::track::RemoteTrack::Audio(_) => {
                        log::info!(
                            "handle_room_events: Stopping audio stream for participant: {}",
                            participant_identity
                        );

                        let mut participants_guard = participants.write().unwrap();
                        if let Some(info) = participants_guard.get_mut(&participant_identity) {
                            info.stop_audio_stream();
                        }
                    }
                }
            }
            RoomEvent::TrackUnpublished {
                publication,
                participant,
            } => {
                log::info!(
                    "handle_room_events: Track unpublished from {}: {} ({:?})",
                    participant.identity(),
                    publication.name(),
                    publication.kind()
                );
                if publication.source() == TrackSource::Camera {
                    update_camera_quality(&inner).await;
                }
            }
            RoomEvent::ConnectionQualityChanged {
                quality,
                participant,
            } if participant.identity().as_str() == user_identity => {
                log::info!("Connection quality changed: {:?}", quality);
                *connection_quality.lock().unwrap() = Some(quality);
            }
            RoomEvent::Reconnecting => {
                log::warn!("handle_room_events: connection lost, reconnecting");
            }
            RoomEvent::Disconnected { reason } => {
                log::warn!("handle_room_events: disconnected: {reason:?}");
            }
            RoomEvent::Reconnected => {
                log::info!("handle_room_events: reconnected");
                let _ = service_command_tx
                    .send(RoomServiceCommand::PublishAppVeilSnapshot { force: true });
                // A full reconnect joins as a new session, which lost our announcement.
                let _ = service_command_tx.send(RoomServiceCommand::AnnounceHevcSupport);
            }
            _ => {}
        }
    }
    log::info!("handle_room_events: ended")
}

#[cfg(test)]
mod connect_gate_tests {
    use super::ConnectGate;
    use tokio::sync::oneshot;

    #[test]
    fn queued_create_room_superseded_by_destroy_is_not_armed() {
        let mut gate = ConnectGate::default();
        let generation = gate.begin();
        // CallEnd while the CreateRoom is still queued behind a slow DestroyRoom.
        gate.invalidate();
        let (tx, mut rx) = oneshot::channel::<()>();
        assert!(!gate.arm(generation, vec![tx]));
        assert!(rx.try_recv().is_err(), "sender dropped: connect cancelled");
        assert!(!gate.is_current(generation));
    }

    #[test]
    fn destroy_supersedes_a_slow_command_of_the_call() {
        let mut gate = ConnectGate::default();
        let generation = gate.begin();
        assert!(gate.arm(generation, Vec::new()));
        let mut during_call = gate.superseded();
        assert!(matches!(
            during_call.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        gate.invalidate();
        assert!(matches!(
            during_call.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        // A command dequeued after the hang-up gives way at once.
        let mut after_hang_up = gate.superseded();
        assert!(matches!(
            after_hang_up.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        // Until the next call connects.
        let generation = gate.begin();
        assert!(gate.arm(generation, Vec::new()));
        let mut next_call = gate.superseded();
        assert!(matches!(
            next_call.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn finished_commands_do_not_pile_up_in_the_gate() {
        let mut gate = ConnectGate::default();
        let generation = gate.begin();
        assert!(gate.arm(generation, Vec::new()));
        for _ in 0..10 {
            drop(gate.superseded());
        }
        let _pending = gate.superseded();
        assert_eq!(gate.cancel_connect.len(), 1);
    }

    #[test]
    fn destroy_cancels_an_armed_connect() {
        let mut gate = ConnectGate::default();
        let generation = gate.begin();
        let (tx, mut rx) = oneshot::channel::<()>();
        assert!(gate.arm(generation, vec![tx]));
        assert!(gate.is_current(generation));
        gate.invalidate();
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert!(!gate.is_current(generation));
    }

    #[test]
    fn newer_create_room_cancels_an_armed_connect() {
        let mut gate = ConnectGate::default();
        let first = gate.begin();
        let (tx, mut rx) = oneshot::channel::<()>();
        assert!(gate.arm(first, vec![tx]));
        let second = gate.begin();
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert!(!gate.is_current(first));
        assert!(gate.arm(second, vec![]));
    }

    #[test]
    fn newer_create_room_supersedes_older_queued_one() {
        let mut gate = ConnectGate::default();
        let first = gate.begin();
        gate.invalidate();
        let second = gate.begin();
        assert!(!gate.arm(first, vec![]));
        assert!(gate.arm(second, vec![]));
    }
}

#[cfg(test)]
mod app_veil_tests {
    use super::*;

    fn window(frame: NormalizedRect, visible_fragments: Vec<NormalizedRect>) -> AppVeilWindow {
        AppVeilWindow {
            frame,
            visible_fragments,
        }
    }

    fn snapshot(windows: Vec<AppVeilWindow>) -> AppVeilSnapshot {
        AppVeilSnapshot {
            windows,
            keyboard_input_blocked: false,
        }
    }

    #[test]
    fn app_veil_snapshot_round_trips_without_client_event_wrapper() {
        let rect = NormalizedRect {
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
        };
        let expected = snapshot(vec![window(rect, vec![rect])]);
        let payload = serde_json::to_vec(&expected).unwrap();

        assert_eq!(
            serde_json::from_slice::<AppVeilSnapshot>(&payload).unwrap(),
            expected
        );
        assert!(!String::from_utf8(payload).unwrap().contains("type"));
    }

    #[test]
    fn app_veil_snapshot_sanitizes_untrusted_rectangles() {
        let sanitized = sanitize_app_veil_snapshot(snapshot(vec![window(
            NormalizedRect {
                x: -0.1,
                y: 0.8,
                width: 0.4,
                height: 0.4,
            },
            vec![
                NormalizedRect {
                    x: -0.1,
                    y: 0.8,
                    width: 0.4,
                    height: 0.4,
                },
                NormalizedRect {
                    x: f32::NAN,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                NormalizedRect {
                    x: 0.2,
                    y: 0.2,
                    width: 0.0,
                    height: 0.5,
                },
                NormalizedRect {
                    x: 2.0,
                    y: 2.0,
                    width: 1.0,
                    height: 1.0,
                },
            ],
        )]));

        assert_eq!(sanitized.windows.len(), 1);
        let rect = sanitized.windows[0].frame;
        assert_eq!((rect.x, rect.y), (0.0, 0.8));
        assert!((rect.width - 0.3).abs() < f32::EPSILON);
        assert!((rect.height - 0.2).abs() < f32::EPSILON);
        assert_eq!(sanitized.windows[0].visible_fragments, vec![rect]);
    }

    #[test]
    fn app_veil_identity_matching_ignores_media_suffix() {
        assert!(participant_identities_match("person:audio", "person:video"));
        assert!(participant_identities_match("person", "person:audio"));
        assert!(!participant_identities_match(
            "person-a:audio",
            "person-b:video"
        ));
    }

    #[test]
    fn app_veil_publication_deduplicates_unless_forced() {
        let current = snapshot(vec![]);
        let rect = NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        let changed = snapshot(vec![window(rect, vec![rect])]);

        assert!(!should_publish_app_veil_snapshot(
            false,
            Some(&current),
            &current
        ));
        assert!(should_publish_app_veil_snapshot(
            false,
            Some(&current),
            &changed
        ));
        assert!(should_publish_app_veil_snapshot(
            true,
            Some(&current),
            &current
        ));
    }
}

#[cfg(test)]
mod effect_packet_tests {
    use super::*;

    #[test]
    fn old_clients_cannot_parse_an_effect_packet_as_a_client_event() {
        // Old clients run unknown topics through the generic ClientEvent parse, which
        // fails and `continue`s: only that packet is dropped.
        let packet = encode_effect_packet(crate::effects::EFFECTS[0].id);
        assert!(serde_json::from_slice::<ClientEvent>(&packet).is_err());
        assert!(parse_effect_packet(&packet).is_some());
    }

    #[test]
    fn effect_topic_is_distinct_from_existing_topics() {
        for topic in [
            TOPIC_SHARER_LOCATION,
            TOPIC_REMOTE_CONTROL_ENABLED,
            TOPIC_PARTICIPANT_IN_CONTROL,
            TOPIC_TICK_RESPONSE,
            TOPIC_DRAW,
            TOPIC_APP_VEIL,
            TOPIC_BANDWIDTH_MODE,
        ] {
            assert_ne!(topic, TOPIC_EFFECT);
        }
    }
}

#[cfg(test)]
mod screen_share_codec_tests {
    use super::*;

    const OWN_VIDEO: &str = "room:r:me:video";

    #[test]
    fn watchers_are_other_users_main_participants_and_browsers() {
        assert!(is_screen_share_watcher("room:r:alice:audio", OWN_VIDEO));
        // A browser viewer on the web app's Room page joins with the audio token too.
        assert!(is_screen_share_watcher("room:r:bob:audio", OWN_VIDEO));
        assert!(is_screen_share_watcher("someone-else", OWN_VIDEO));
    }

    #[test]
    fn video_participants_and_our_own_main_participant_are_not_watchers() {
        assert!(!is_screen_share_watcher("room:r:alice:video", OWN_VIDEO));
        assert!(!is_screen_share_watcher(OWN_VIDEO, OWN_VIDEO));
        assert!(!is_screen_share_watcher("room:r:me:audio", OWN_VIDEO));
    }

    #[test]
    fn livekit_codecs() {
        assert_eq!(
            livekit_video_codec(ScreenShareCodec::H265),
            VideoCodec::H265
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            livekit_video_codec(ScreenShareCodec::H264),
            VideoCodec::H264
        );
    }
}
