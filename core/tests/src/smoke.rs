//! Self-checking scenarios. Each one drives core over its socket the way Tauri does, joins the
//! LiveKit room as other participants where needed, and checks what core and the room report.
//! A scenario passes or fails on its own, with the reason; nobody has to watch the screen.
//!
//! Core exits when its client disconnects, so each scenario needs a fresh core:
//! `core/tests/smoke.sh` starts one per scenario.

use crate::ipc::{CoreConn, Step, REQUEST_TIMEOUT};
use crate::livekit_utils::{generate_participant_token, participant_identity};
use crate::screenshare_client;
use clap::ValueEnum;
use futures::StreamExt;
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::prelude::VideoResolution as WebrtcVideoResolution;
use livekit::webrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_source::RtcVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;
use socket_lib::{
    BandwidthModeState, CallId, CoreParticipantState, Message, RoomConnectionFailedMessage,
};
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

/// User the core under test joins as.
const CORE_USER: &str = "Smoke Core";
/// Other participants the scenarios add to the room.
const OBSERVER: &str = "Smoke Observer";
const REMOTE: &str = "Smoke Remote";
const FAKE_SHARER: &str = "Smoke Fake Sharer";

/// Data topic and payload core uses to negotiate low-bandwidth mode (`room_service.rs`).
const TOPIC_BANDWIDTH_MODE: &str = "bandwidth_mode";

pub(crate) const ROOM_EVENT_TIMEOUT: Duration = Duration::from_secs(15);
/// Above this, core is busy with something after the call ended (the call-end CPU bug).
const MAX_IDLE_CPU: f64 = 0.25;

#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Scenario {
    /// Join a call, check core in the room and its participant list, end it, check core leaves
    CallLifecycle,
    /// Five calls back to back, each joined and ended
    CallRestart,
    /// End calls before their room connects, then start one that must connect and stay up
    CallEndRace,
    /// A late CallEnd for the previous call must not end the current one
    StaleCallEnd,
    /// Low-bandwidth negotiation: local toggle, request sent to others, remote request, requester leaving
    Bandwidth,
    /// Core views a fake sharer's screen through a mute/unmute storm and keeps answering
    ViewerHang,
    /// Core shares the screen and another participant receives frames (needs Screen Recording)
    Screenshare,
    /// LiveKit traffic cut for 10 s mid-call; the call must recover (needs sudo, see netem.sh)
    NetworkDrop,
}

impl Scenario {
    fn name(self) -> String {
        self.to_possible_value().unwrap().get_name().to_string()
    }
}

/// Runs `scenario` against the core at `--socket-path`. `core_pid` enables the idle CPU check.
pub async fn run(scenario: Scenario, core_pid: Option<u32>) -> io::Result<()> {
    let started = Instant::now();
    let result = run_scenario(scenario, core_pid).await;
    let elapsed = started.elapsed().as_secs_f64();
    match &result {
        Ok(()) => println!("PASS {} ({elapsed:.1}s)", scenario.name()),
        Err(e) => println!("FAIL {} ({elapsed:.1}s): {e}", scenario.name()),
    }
    result
}

async fn run_scenario(scenario: Scenario, core_pid: Option<u32>) -> io::Result<()> {
    let conn = CoreConn::connect_with_livekit_url()?;
    match scenario {
        Scenario::CallLifecycle => call_lifecycle(&conn, core_pid).await,
        Scenario::CallRestart => call_restart(&conn),
        Scenario::CallEndRace => call_end_race(&conn).await,
        Scenario::StaleCallEnd => stale_call_end(&conn).await,
        Scenario::Bandwidth => bandwidth(&conn).await,
        Scenario::ViewerHang => viewer_hang(&conn, core_pid).await,
        Scenario::Screenshare => screenshare(&conn, core_pid).await,
        Scenario::NetworkDrop => network_drop(&conn).await,
    }
}

async fn call_lifecycle(conn: &CoreConn, core_pid: Option<u32>) -> io::Result<()> {
    let mut observer = Participant::join(OBSERVER).await?;

    step("start call");
    let call_id = conn.start_call(CORE_USER)?;
    let snapshot = conn.wait_room_ready(call_id)?;
    check(
        snapshot.iter().any(|p| p.identity == "local"),
        format!("core's participant list has no local entry: {snapshot:?}"),
    )?;
    // Both follow the room-ready snapshot, in no guaranteed order.
    let observer_identity = participant_identity(OBSERVER, "audio");
    let lists_observer = |participants: &[CoreParticipantState]| {
        participants.iter().any(|p| p.identity == observer_identity)
    };
    let mut observer_listed = lists_observer(&snapshot);
    let mut initial_bandwidth = None;
    conn.wait_for(
        ROOM_EVENT_TIMEOUT,
        "the observer in core's participant list and the initial BandwidthModeState",
        |message| {
            match message {
                Message::ParticipantsSnapshot(participants) => {
                    observer_listed |= lists_observer(&participants)
                }
                Message::BandwidthModeState(state) => {
                    initial_bandwidth.get_or_insert(state);
                }
                _ => {}
            }
            if observer_listed && initial_bandwidth.is_some() {
                Step::Done(())
            } else {
                Step::Skip
            }
        },
    )?;
    let state = initial_bandwidth.unwrap_or_default();
    check(
        !state.active,
        format!("low bandwidth active at call start: {state:?}"),
    )?;

    step("core is in the room");
    for track in ["audio", "video"] {
        observer
            .wait_present(&participant_identity(CORE_USER, track))
            .await?;
    }
    step(&format!(
        "core answers in {:?}",
        conn.probe(REQUEST_TIMEOUT)?
    ));

    step("end call");
    conn.end_call(call_id)?;
    observer
        .wait_gone(&participant_identity(CORE_USER, "audio"))
        .await?;
    check_idle_cpu(core_pid)?;
    conn.probe(REQUEST_TIMEOUT)?;
    Ok(())
}

fn call_restart(conn: &CoreConn) -> io::Result<()> {
    for round in 1..=5 {
        step(&format!("call {round}: join"));
        let call_id = conn.join_call(CORE_USER)?;
        step(&format!("call {round}: end"));
        conn.end_call(call_id)?;
    }
    conn.probe(REQUEST_TIMEOUT)?;
    Ok(())
}

async fn call_end_race(conn: &CoreConn) -> io::Result<()> {
    let mut observer = Participant::join(OBSERVER).await?;

    for round in 1..=3 {
        step(&format!(
            "call {round}: start and end before the room connects"
        ));
        let call_id = conn.start_call(CORE_USER)?;
        conn.end_call(call_id)?;
    }

    step("start the call that must stay up");
    let call_id = conn.join_call(CORE_USER)?;
    expect_call_stays_up(conn, call_id, Duration::from_secs(5))?;
    observer
        .wait_present(&participant_identity(CORE_USER, "audio"))
        .await?;
    conn.end_call(call_id)?;
    Ok(())
}

async fn stale_call_end(conn: &CoreConn) -> io::Result<()> {
    let mut observer = Participant::join(OBSERVER).await?;

    step("call A: join and end");
    let old_call = conn.join_call(CORE_USER)?;
    conn.end_call(old_call)?;

    step("call B: join");
    let call_id = conn.join_call(CORE_USER)?;

    step("resend CallEnd for call A");
    conn.send(Message::CallEnd(Some(old_call)))?;
    conn.wait_for(
        REQUEST_TIMEOUT,
        "CallEnded for call A",
        |message| match message {
            Message::CallEnded(id) if id == old_call => Step::Done(()),
            Message::CallEnded(id) if id == call_id => Step::Fail("core ended call B".into()),
            _ => Step::Skip,
        },
    )?;
    expect_call_stays_up(conn, call_id, Duration::from_secs(3))?;
    let core_identity = participant_identity(CORE_USER, "audio");
    observer.wait_present(&core_identity).await?;
    check(
        observer.is_present(&core_identity),
        "core left the room after the stale CallEnd",
    )?;
    conn.probe(REQUEST_TIMEOUT)?;
    conn.end_call(call_id)?;
    Ok(())
}

async fn bandwidth(conn: &CoreConn) -> io::Result<()> {
    let mut remote = Participant::join(REMOTE).await?;

    let call_id = conn.join_call(CORE_USER)?;
    let state = wait_bandwidth_state(conn, "initial BandwidthModeState", |_| true)?;
    check(
        !state.active,
        format!("low bandwidth active at call start: {state:?}"),
    )?;

    step("local request");
    conn.send(Message::SetCallLowBandwidth(true))?;
    wait_bandwidth_state(conn, "local request active", |s| {
        s.active && s.local_requested
    })?;
    step("core tells the others");
    remote
        .wait_event("core's low-bandwidth request", |event| match event {
            RoomEvent::DataReceived { payload, topic, .. }
                if topic.as_deref() == Some(TOPIC_BANDWIDTH_MODE) =>
            {
                serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .filter(|request| request["low_bandwidth"] == true)
                    .map(|_| ())
            }
            _ => None,
        })
        .await?;

    step("local request withdrawn");
    conn.send(Message::SetCallLowBandwidth(false))?;
    wait_bandwidth_state(conn, "local request withdrawn", |s| {
        !s.active && !s.local_requested
    })?;

    step("remote request");
    remote.request_low_bandwidth(true).await?;
    wait_bandwidth_state(conn, "remote request active", |s| {
        s.active && s.requested_by.iter().any(|r| r == REMOTE)
    })?;

    step("requester leaves");
    remote.leave().await;
    wait_bandwidth_state(conn, "request dropped when the requester left", |s| {
        !s.active && s.requested_by.is_empty()
    })?;

    conn.end_call(call_id)?;
    Ok(())
}

async fn viewer_hang(conn: &CoreConn, core_pid: Option<u32>) -> io::Result<()> {
    const STORM_CYCLES: usize = 20;
    const LIVENESS_WINDOW: Duration = Duration::from_secs(20);

    let call_id = conn.join_call(CORE_USER)?;

    step("fake sharer publishes a screen share");
    // Core resolves a sharer's name through its audio identity, so join with both, like a client.
    let sharer_audio = Participant::join(FAKE_SHARER).await?;
    let sharer = Participant::join_track(FAKE_SHARER, "video").await?;
    let source = NativeVideoSource::new(
        WebrtcVideoResolution {
            width: 1280,
            height: 720,
        },
        true,
    );
    let pusher = tokio::spawn(push_frames(source.clone()));
    let track = LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source));
    // Like the app: the track is published muted at call start and unmuted to share.
    track.mute();
    let publication = sharer
        .room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: VideoCodec::H264,
                video_encoding: Some(VideoEncoding {
                    max_bitrate: 2_000_000,
                    max_framerate: 15.0,
                }),
                simulcast: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| io::Error::other(format!("publishing the fake screen share failed: {e:?}")))?;
    publication.unmute();

    conn.wait_for(
        ROOM_EVENT_TIMEOUT,
        "core seeing the remote share",
        |message| match message {
            Message::ParticipantsSnapshot(participants) if remote_sharing(&participants) => {
                Step::Done(())
            }
            _ => Step::Skip,
        },
    )?;

    step(&format!("mute/unmute storm ({STORM_CYCLES} cycles)"));
    for _ in 0..STORM_CYCLES {
        publication.mute();
        tokio::time::sleep(Duration::from_millis(100)).await;
        publication.unmute();
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.probe(REQUEST_TIMEOUT).map_err(|e| {
            io::Error::other(format!("core stopped answering during the storm: {e}"))
        })?;
    }

    step(&format!("core keeps answering for {LIVENESS_WINDOW:?}"));
    let probing_since = Instant::now();
    let mut slowest = Duration::ZERO;
    while probing_since.elapsed() < LIVENESS_WINDOW {
        let round_trip = conn.probe(REQUEST_TIMEOUT).map_err(|e| {
            io::Error::other(format!("core stopped answering after the storm: {e}"))
        })?;
        slowest = slowest.max(round_trip);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    step(&format!("slowest answer {slowest:?}"));

    pusher.abort();
    sharer.leave().await;
    sharer_audio.leave().await;
    conn.end_call(call_id)?;
    check_idle_cpu(core_pid)?;
    Ok(())
}

async fn screenshare(conn: &CoreConn, core_pid: Option<u32>) -> io::Result<()> {
    const FRAME_WINDOW: Duration = Duration::from_secs(10);

    let mut observer = Participant::join(OBSERVER).await?;
    let call_id = conn.join_call(CORE_USER)?;

    step("start screen share");
    let snapshot = conn.start_screenshare(screenshare_client::screen_id(), 1920.0, 1080.0)?;
    check(
        snapshot.as_deref().is_some_and(local_sharing),
        format!("core's participant list does not show the local share: {snapshot:?}"),
    )?;

    step("observer receives the share");
    let sharer_video = participant_identity(CORE_USER, "video");
    let track = observer
        .wait_event("screen share track", |event| match event {
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(track),
                publication,
                participant,
            } if publication.source() == TrackSource::Screenshare => {
                Some((track, participant.identity().to_string()))
            }
            _ => None,
        })
        .await?;
    check(
        track.1 == sharer_video,
        format!(
            "screen share came from {}, expected {sharer_video}",
            track.1
        ),
    )?;
    let frames = count_frames(track.0, FRAME_WINDOW).await;
    step(&format!(
        "{} frames in {FRAME_WINDOW:?}, first after {:?}, {}x{}",
        frames.count, frames.first_after, frames.width, frames.height
    ));
    check(frames.count > 0, "the observer received no frames")?;

    step("stop screen share");
    // Core publishes the track muted at call start, so earlier mute events are stale.
    observer.drain();
    conn.send(Message::StopScreenshare)?;
    conn.wait_for(
        REQUEST_TIMEOUT,
        "participant list without the local share",
        |message| match message {
            Message::ParticipantsSnapshot(participants) if !local_sharing(&participants) => {
                Step::Done(())
            }
            _ => Step::Skip,
        },
    )?;
    observer
        .wait_event("screen share track gone", |event| match event {
            RoomEvent::TrackUnpublished { publication, .. }
                if publication.source() == TrackSource::Screenshare =>
            {
                Some(())
            }
            RoomEvent::TrackMuted { publication, .. }
                if publication.source() == TrackSource::Screenshare =>
            {
                Some(())
            }
            _ => None,
        })
        .await?;

    conn.end_call(call_id)?;
    check_idle_cpu(core_pid)?;
    Ok(())
}

async fn network_drop(conn: &CoreConn) -> io::Result<()> {
    const OUTAGE_SECONDS: &str = "10";
    const RECOVERY: Duration = Duration::from_secs(20);
    let netem = concat!(env!("CARGO_MANIFEST_DIR"), "/netem.sh");

    let mut observer = Participant::join(OBSERVER).await?;
    let call_id = conn.join_call(CORE_USER)?;
    let core_identity = participant_identity(CORE_USER, "audio");
    observer.wait_present(&core_identity).await?;

    step(&format!("cut LiveKit traffic for {OUTAGE_SECONDS} s"));
    // -n: never prompt; smoke.sh asks for the password up front.
    let status = Command::new("sudo")
        .args(["-n", netem, "drop", "--for", OUTAGE_SECONDS])
        .status()?;
    check(
        status.success(),
        format!(
            "`sudo -n {netem} drop` failed ({status}); run through smoke.sh, which caches sudo"
        ),
    )?;

    step("call recovers");
    expect_call_stays_up(conn, call_id, RECOVERY)?;
    observer.wait_present(&core_identity).await?;
    conn.probe(REQUEST_TIMEOUT)?;
    conn.end_call(call_id)?;
    Ok(())
}

/// Another participant in the room, joined over LiveKit directly.
pub(crate) struct Participant {
    pub(crate) room: Room,
    events: UnboundedReceiver<RoomEvent>,
}

impl Participant {
    pub(crate) async fn join(user: &str) -> io::Result<Self> {
        Self::join_track(user, "audio").await
    }

    pub(crate) async fn join_track(user: &str, track: &str) -> io::Result<Self> {
        let url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL environment variable not set");
        let token = generate_participant_token(user, track);
        let (room, events) = Room::connect(&url, &token, RoomOptions::default())
            .await
            .map_err(|e| io::Error::other(format!("{user} could not join the room: {e}")))?;
        Ok(Self { room, events })
    }

    pub(crate) async fn wait_event<T>(
        &mut self,
        what: &str,
        mut pick: impl FnMut(RoomEvent) -> Option<T>,
    ) -> io::Result<T> {
        let events = &mut self.events;
        let wait = async {
            while let Some(event) = events.recv().await {
                if let Some(value) = pick(event) {
                    return Some(value);
                }
            }
            None
        };
        match tokio::time::timeout(ROOM_EVENT_TIMEOUT, wait).await {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(io::Error::other(format!(
                "room closed while waiting for {what}"
            ))),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {ROOM_EVENT_TIMEOUT:?} waiting for {what}"),
            )),
        }
    }

    /// Drops the events received so far.
    fn drain(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    fn is_present(&self, identity: &str) -> bool {
        self.room
            .remote_participants()
            .values()
            .any(|p| p.identity().to_string() == identity)
    }

    async fn wait_present(&mut self, identity: &str) -> io::Result<()> {
        if self.is_present(identity) {
            return Ok(());
        }
        self.wait_event(&format!("{identity} to join"), |event| match event {
            RoomEvent::ParticipantConnected(p) if p.identity().to_string() == identity => Some(()),
            _ => None,
        })
        .await
    }

    async fn wait_gone(&mut self, identity: &str) -> io::Result<()> {
        if !self.is_present(identity) {
            return Ok(());
        }
        self.wait_event(&format!("{identity} to leave"), |event| match event {
            RoomEvent::ParticipantDisconnected(p) if p.identity().to_string() == identity => {
                Some(())
            }
            _ => None,
        })
        .await
    }

    async fn request_low_bandwidth(&self, low_bandwidth: bool) -> io::Result<()> {
        self.room
            .local_participant()
            .publish_data(DataPacket {
                payload: serde_json::to_vec(&serde_json::json!({ "low_bandwidth": low_bandwidth }))
                    .unwrap(),
                reliable: true,
                topic: Some(TOPIC_BANDWIDTH_MODE.to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| io::Error::other(format!("publishing the bandwidth request failed: {e}")))
    }

    pub(crate) async fn leave(self) {
        let _ = self.room.close().await;
    }
}

#[derive(Default)]
struct FrameStats {
    count: u32,
    first_after: Option<Duration>,
    width: u32,
    height: u32,
}

async fn count_frames(track: RemoteVideoTrack, window: Duration) -> FrameStats {
    let mut stream = NativeVideoStream::new(track.rtc_track());
    let started = Instant::now();
    let mut stats = FrameStats::default();
    while let Ok(Some(frame)) =
        tokio::time::timeout(window.saturating_sub(started.elapsed()), stream.next()).await
    {
        if stats.count == 0 {
            stats.first_after = Some(started.elapsed());
        }
        stats.count += 1;
        stats.width = frame.buffer.width();
        stats.height = frame.buffer.height();
    }
    stream.close();
    stats
}

/// Feeds the fake screen share 15 frames per second with changing content.
async fn push_frames(source: NativeVideoSource) {
    let mut luma: u8 = 0;
    let mut interval = tokio::time::interval(Duration::from_millis(66));
    loop {
        interval.tick().await;
        let mut buffer = I420Buffer::new(1280, 720);
        let (y, u, v) = buffer.data_mut();
        y.fill(luma);
        u.fill(128);
        v.fill(128);
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer,
        });
        luma = luma.wrapping_add(4);
    }
}

fn local_sharing(participants: &[CoreParticipantState]) -> bool {
    participants
        .iter()
        .any(|p| p.identity == "local" && p.is_screensharing)
}

fn remote_sharing(participants: &[CoreParticipantState]) -> bool {
    participants
        .iter()
        .any(|p| p.identity != "local" && p.is_screensharing)
}

fn wait_bandwidth_state(
    conn: &CoreConn,
    what: &str,
    mut matches: impl FnMut(&BandwidthModeState) -> bool,
) -> io::Result<BandwidthModeState> {
    conn.wait_for(ROOM_EVENT_TIMEOUT, what, |message| match message {
        Message::BandwidthModeState(state) if matches(&state) => Step::Done(state),
        _ => Step::Skip,
    })
}

/// Fails if core ends `call_id` or loses its room within `duration`.
fn expect_call_stays_up(conn: &CoreConn, call_id: CallId, duration: Duration) -> io::Result<()> {
    conn.expect_none(duration, "the call to stay up", |message| match message {
        Message::CallEnded(id) => *id == call_id,
        Message::RoomConnectionFailed(RoomConnectionFailedMessage { call_id: id, .. }) => {
            *id == call_id
        }
        _ => false,
    })
}

/// Fails if core keeps using CPU after a call ended (`core_pid` unset: skipped).
fn check_idle_cpu(core_pid: Option<u32>) -> io::Result<()> {
    const SETTLE: Duration = Duration::from_secs(3);
    const WINDOW: Duration = Duration::from_secs(5);

    let Some(pid) = core_pid else {
        step("idle CPU check skipped (no --core-pid)");
        return Ok(());
    };
    std::thread::sleep(SETTLE);
    let before = cpu_seconds(pid)?;
    std::thread::sleep(WINDOW);
    let used = (cpu_seconds(pid)? - before) / WINDOW.as_secs_f64();
    step(&format!("core CPU after the call: {:.0}%", used * 100.0));
    check(
        used < MAX_IDLE_CPU,
        format!(
            "core uses {:.0}% CPU {:?} after the call ended (limit {:.0}%)",
            used * 100.0,
            SETTLE,
            MAX_IDLE_CPU * 100.0
        ),
    )
}

/// CPU time `pid` has used so far, from `ps` ("[[hh:]mm:]ss.cc").
fn cpu_seconds(pid: u32) -> io::Result<f64> {
    let output = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    text.split(':')
        .try_fold(0.0, |total, part| {
            part.trim().parse::<f64>().map(|value| total * 60.0 + value)
        })
        .map_err(|_| io::Error::other(format!("can't read CPU time of core (pid {pid}): {text:?}")))
}

fn check(ok: bool, failure: impl Into<String>) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(failure.into()))
    }
}

fn step(description: &str) {
    println!("  - {description}");
}
