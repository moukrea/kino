//! Linux mpv subprocess driver (PRD §F-015, ADR-011 + ADR-108).
//!
//! Spawns `mpv --input-ipc-server=<socket>` and drives it via the JSON
//! IPC protocol parsed in [`crate::ipc`]. The driver is single-session
//! — opening a new file while one is loaded replaces the in-flight
//! session by reloading on the same player process.
//!
//! ### Translation of PRD §F-015 Linux requirements
//!
//! * mpv config from `crates/kino-server/assets/mpv.conf` is loaded by
//!   passing `--include=<path>` when the host knows the asset
//!   location; otherwise the driver applies the same set as
//!   command-line flags so test environments without the assets bundle
//!   still get PRD-compliant behaviour.
//! * Position ticks: the driver `observe_property time-pos` and
//!   forwards each update as a [`PlayerEvent::Position`]. PRD §8 caps
//!   the cadence at 5 s, but mpv emits as fast as the demuxer ticks;
//!   the driver rate-limits to one tick per
//!   [`PLAYER_POSITION_INTERVAL_S`] so the Tauri host (and downstream
//!   F-012 / F-014 consumers) see exactly one tick per spec interval
//!   plus immediate ticks on seeks / pause / resume.
//! * Buffer underruns: observed via `paused-for-cache`; mapped to
//!   [`PlayerState::Buffering`].
//! * End-of-file: observed via `end-file`; mapped to a terminal
//!   [`PlayerEvent::Exit { reached_eof: true, .. }`].
//! * Tracks: observed via `track-list`; reshaped through
//!   [`TrackList::from_mpv_tracks`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::Instant;
use uuid::Uuid;

use crate::error::PlayerError;
use crate::event::{PlayerEvent, PositionTick};
use crate::handle::{OpenRequest, PlayerHandle};
use crate::ipc::{parse_frame, Command as IpcCommand, Event as IpcEvent, Frame};
use crate::state::{PlayerSnapshot, PlayerState};
use crate::tracks::TrackList;

/// Player tick cadence in seconds (PRD §8 `PLAYER_POSITION_INTERVAL_S`).
/// Duplicated here rather than imported from `kino-core::constants` to
/// keep `kino-player` cycle-free from the workspace crate that depends
/// on this one for type re-exports (`kino-app` pulls both).
const PLAYER_POSITION_INTERVAL_S: f64 = 5.0;

/// How long to wait for mpv's IPC socket to become connectable after we
/// spawn the subprocess. mpv typically opens the socket within ~20ms;
/// 5s is generous head-room for slow / cold sandboxes.
const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Capacity of the event broadcast channel. The Tauri bridge consumes
/// events as fast as they arrive, but jsdom-style unit tests may hold a
/// receiver without polling; a 64-slot buffer absorbs that without
/// turning the receiver into an unbounded memory leak.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Path to the mpv binary. Override with `KINO_MPV_PATH` for sandboxes
/// where mpv ships at a non-standard location.
fn mpv_binary() -> String {
    std::env::var("KINO_MPV_PATH").unwrap_or_else(|_| "mpv".to_string())
}

/// Builder for the mpv driver. Test code uses the builder to point at a
/// fake mpv (`with_binary`) without setting environment variables.
#[derive(Debug, Clone)]
pub struct MpvBuilder {
    binary: String,
    include_config: Option<PathBuf>,
    extra_args: Vec<String>,
}

impl Default for MpvBuilder {
    fn default() -> Self {
        Self {
            binary: mpv_binary(),
            include_config: None,
            extra_args: Vec::new(),
        }
    }
}

impl MpvBuilder {
    /// Override the binary path. Used by tests; production callers leave
    /// the default (`mpv` resolved against `$PATH`) in place.
    #[must_use]
    pub fn with_binary(mut self, path: impl Into<String>) -> Self {
        self.binary = path.into();
        self
    }

    /// Point at a `mpv.conf` to load via `--include`. Optional — the
    /// inline flags applied below are enough for PRD §F-015
    /// compliance.
    #[must_use]
    pub fn with_include_config(mut self, path: PathBuf) -> Self {
        self.include_config = Some(path);
        self
    }

    /// Append additional command-line arguments. Used by tests that
    /// drive a fake mpv with a custom socket-greeting script.
    #[must_use]
    pub fn with_extra_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Spawn an idle mpv process and connect to its JSON-IPC socket.
    /// The driver waits for the socket to be reachable before returning.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Spawn`] if `mpv` isn't on `$PATH`, and
    /// [`PlayerError::Closed`] if the process exits before the socket
    /// becomes connectable.
    pub async fn spawn(self) -> Result<MpvPlayer, PlayerError> {
        let socket_path = mint_socket_path();
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--idle=yes")
            .arg("--no-terminal")
            .arg("--force-window=immediate")
            .arg("--keep-open=yes")
            .arg("--hwdec=auto-safe")
            .arg("--cache=yes")
            .arg("--demuxer-max-bytes=200MiB")
            .arg("--demuxer-readahead-secs=20")
            .arg("--audio-spdif=ac3,dts,eac3,truehd,dts-hd")
            .arg("--sub-auto=fuzzy")
            .arg("--sub-ass=yes")
            .arg(format!("--input-ipc-server={}", socket_path.display()));
        if let Some(include) = &self.include_config {
            cmd.arg(format!("--include={}", include.display()));
        }
        for extra in &self.extra_args {
            cmd.arg(extra);
        }
        cmd.kill_on_drop(true);

        let child = cmd.spawn().map_err(PlayerError::Spawn)?;

        // Wait for the socket to appear and be connectable. mpv creates
        // the socket asynchronously after spawn, so we poll with a tiny
        // backoff.
        let stream = wait_for_socket(&socket_path).await?;
        MpvPlayer::start(child, stream, socket_path).await
    }
}

fn mint_socket_path() -> PathBuf {
    let id = Uuid::new_v4();
    std::env::temp_dir().join(format!("kino-mpv-{id}.sock"))
}

async fn wait_for_socket(path: &std::path::Path) -> Result<UnixStream, PlayerError> {
    let deadline = Instant::now() + IPC_CONNECT_TIMEOUT;
    let mut backoff_ms = 10u64;
    loop {
        match UnixStream::connect(path).await {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms.saturating_mul(2)).min(200);
            }
            Err(e) => return Err(PlayerError::Closed(format!("socket not ready: {e}"))),
        }
    }
}

/// Type alias for the `request_id` → reply-channel map shared between
/// the writer and reader tasks. The writer inserts before issuing a
/// command, the reader removes when the response arrives.
type PendingReplies =
    Arc<Mutex<std::collections::HashMap<u64, oneshot::Sender<Result<Value, PlayerError>>>>>;

/// Driver implementing [`PlayerHandle`] on top of the mpv subprocess
/// and its JSON-IPC socket.
#[derive(Debug)]
pub struct MpvPlayer {
    inner: Arc<MpvInner>,
}

#[derive(Debug)]
struct MpvInner {
    /// Outbound channel: every IPC command is sent through here so the
    /// writer task owns sole access to the socket write half.
    cmd_tx: mpsc::UnboundedSender<OutboundCommand>,
    /// Broadcast of [`PlayerEvent`] to all subscribers.
    events: broadcast::Sender<PlayerEvent>,
    /// Latest snapshot — kept in sync by the reader task.
    snapshot: Mutex<PlayerSnapshot>,
    /// Latest track list — kept in sync by the reader task.
    tracks: Mutex<TrackList>,
    /// Shutdown signal for the writer task.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Path to the socket so it can be cleaned up on drop.
    socket_path: PathBuf,
    /// Counter for `request_id` generation.
    next_id: std::sync::atomic::AtomicU64,
}

impl Drop for MpvInner {
    fn drop(&mut self) {
        // Best-effort socket-file cleanup; ignore I/O errors here
        // because the process is going down anyway.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[derive(Debug)]
struct OutboundCommand {
    args: Vec<Value>,
    /// `Some` when the caller wants to wait for the response.
    reply_tx: Option<oneshot::Sender<Result<Value, PlayerError>>>,
}

impl MpvPlayer {
    /// Convenience: spawn with default settings.
    pub async fn spawn() -> Result<Self, PlayerError> {
        MpvBuilder::default().spawn().await
    }

    async fn start(
        child: Child,
        stream: UnixStream,
        socket_path: PathBuf,
    ) -> Result<Self, PlayerError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<OutboundCommand>();
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let inner = Arc::new(MpvInner {
            cmd_tx,
            events: events_tx,
            snapshot: Mutex::new(PlayerSnapshot::idle(String::new())),
            tracks: Mutex::new(TrackList::default()),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            socket_path,
            next_id: std::sync::atomic::AtomicU64::new(1),
        });

        let (read_half, write_half) = stream.into_split();
        let pending: PendingReplies = Arc::new(Mutex::new(std::collections::HashMap::new()));

        // Spawn the reader task (parses incoming frames, dispatches to
        // pending replies and updates snapshot / events).
        let reader_inner = inner.clone();
        let reader_pending = pending.clone();
        tokio::spawn(reader_task(read_half, reader_inner, reader_pending));

        // Spawn the writer task (drains cmd_rx, writes lines to the
        // socket, registers reply senders in pending).
        let writer_inner = inner.clone();
        let writer_pending = pending.clone();
        tokio::spawn(writer_task(
            write_half,
            cmd_rx,
            writer_pending,
            writer_inner,
            shutdown_rx,
            child,
        ));

        // Observe the properties we care about. mpv treats every
        // property change as an event, so we don't need polling.
        let player = Self { inner };
        player
            .observe_property(1, "time-pos")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe time-pos: {e}")))?;
        player
            .observe_property(2, "duration")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe duration: {e}")))?;
        player
            .observe_property(3, "pause")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe pause: {e}")))?;
        player
            .observe_property(4, "paused-for-cache")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe paused-for-cache: {e}")))?;
        player
            .observe_property(5, "eof-reached")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe eof-reached: {e}")))?;
        player
            .observe_property(6, "track-list")
            .await
            .map_err(|e| PlayerError::Closed(format!("observe track-list: {e}")))?;

        Ok(player)
    }

    /// Send a command and wait for its response. Returns the `data`
    /// field on success.
    async fn request(&self, args: Vec<Value>) -> Result<Value, PlayerError> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .cmd_tx
            .send(OutboundCommand {
                args,
                reply_tx: Some(tx),
            })
            .map_err(|_| PlayerError::Closed("driver writer task gone".to_string()))?;
        rx.await
            .map_err(|_| PlayerError::Closed("driver writer task gone".to_string()))?
    }

    /// Send a command, fire-and-forget. Used for events we don't need
    /// to await (e.g. quit on close — the actual completion arrives
    /// via the `shutdown` event).
    fn fire_and_forget(&self, args: Vec<Value>) -> Result<(), PlayerError> {
        self.inner
            .cmd_tx
            .send(OutboundCommand {
                args,
                reply_tx: None,
            })
            .map_err(|_| PlayerError::Closed("driver writer task gone".to_string()))
    }

    async fn observe_property(&self, id: u64, name: &str) -> Result<(), PlayerError> {
        // mpv's `observe_property` takes a client-chosen integer id and
        // a property name. We don't need the id for dispatch (we key on
        // the property name in the reader task) but mpv requires one.
        self.request(vec![json!("observe_property"), json!(id), json!(name)])
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl PlayerHandle for MpvPlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        self.inner
            .snapshot
            .try_lock()
            .map_or_else(|_| PlayerSnapshot::idle(String::new()), |g| g.clone())
    }

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.inner.events.subscribe()
    }

    async fn open(&self, req: OpenRequest) -> Result<(), PlayerError> {
        // Update the snapshot eagerly so the host's first `snapshot()`
        // after `open()` reports the new token (the IPC round-trip
        // below races against a frontend `player_status` call).
        {
            let mut snap = self.inner.snapshot.lock().await;
            snap.token.clone_from(&req.token);
            snap.state = PlayerState::Loading;
            snap.position_s = req.resume_position_s;
            snap.duration_s = req.duration_hint_s.unwrap_or(0.0);
            snap.paused = false;
        }
        let _ = self
            .inner
            .events
            .send(PlayerEvent::state(PlayerState::Loading));

        // mpv `loadfile` accepts a start position as the fourth arg:
        // `loadfile <url> replace 0 start=<seconds>`.
        let mut load_args = vec![json!("loadfile"), json!(req.url), json!("replace")];
        if req.resume_position_s > 0.0 {
            load_args.push(json!(0));
            load_args.push(json!(format!("start={}", req.resume_position_s)));
        }
        self.request(load_args).await.map(|_| ())
    }

    async fn close(&self) -> Result<(), PlayerError> {
        // Capture the final position from the snapshot BEFORE issuing
        // the quit so the Exit event we synthesise below reports the
        // last value we saw rather than racing the shutdown.
        let snap = self.snapshot();

        // Fire-and-forget: the response races with the process exit and
        // mpv may close the socket before serializing the reply. The
        // `shutdown` event arriving on the reader is the authoritative
        // signal.
        let _ = self.fire_and_forget(vec![json!("quit")]);

        // Synthesize the Exit event ourselves so the host's bridge has
        // a deterministic terminal marker even when the IPC socket
        // closes mid-handshake.
        let _ = self.inner.events.send(PlayerEvent::Exit {
            position_s: snap.position_s,
            duration_s: snap.duration_s,
            reached_eof: false,
        });
        // Hand the shutdown signal to the writer task so it tears down
        // its loop and reaps the child.
        if let Some(tx) = self.inner.shutdown_tx.lock().await.take() {
            let _ = tx.send(());
        }
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.request(vec![json!("set"), json!("pause"), json!(paused)])
            .await
            .map(|_| ())
    }

    async fn seek(&self, position_s: f64) -> Result<(), PlayerError> {
        self.request(vec![
            json!("seek"),
            json!(position_s),
            json!("absolute"),
            json!("exact"),
        ])
        .await
        .map(|_| ())
    }

    async fn select_audio_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        let value = track_id.map_or_else(|| json!("no"), |id| json!(id.to_string()));
        self.request(vec![json!("set"), json!("aid"), value])
            .await
            .map(|_| ())
    }

    async fn select_subtitle_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        let value = track_id.map_or_else(|| json!("no"), |id| json!(id.to_string()));
        self.request(vec![json!("set"), json!("sid"), value])
            .await
            .map(|_| ())
    }

    fn tracks(&self) -> TrackList {
        self.inner
            .tracks
            .try_lock()
            .map_or_else(|_| TrackList::default(), |g| g.clone())
    }
}

// ---- background tasks --------------------------------------------------

async fn writer_task(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    mut cmd_rx: mpsc::UnboundedReceiver<OutboundCommand>,
    pending: PendingReplies,
    inner: Arc<MpvInner>,
    mut shutdown_rx: oneshot::Receiver<()>,
    mut child: Child,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                // Allow mpv a tiny grace period to flush stdout on the
                // pre-issued `quit` command, then reap.
                let _ = tokio::time::timeout(
                    Duration::from_millis(200),
                    child.wait(),
                ).await;
                let _ = child.start_kill();
                let _ = child.wait().await;
                break;
            }
            next = cmd_rx.recv() => {
                let Some(OutboundCommand { args, reply_tx }) = next else { break };
                let id = inner
                    .next_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let cmd = IpcCommand::new(id, args);
                let line = cmd.to_line();
                if let Some(reply) = reply_tx {
                    pending.lock().await.insert(id, reply);
                }
                if let Err(e) = write_half.write_all(&line).await {
                    // Surface the wire error to whichever requester was
                    // waiting on this id, then bail.
                    if let Some(reply) = pending.lock().await.remove(&id) {
                        let _ = reply.send(Err(PlayerError::write(e)));
                    }
                    break;
                }
                // Flush is a no-op for UnixStream but documents intent.
                let _ = write_half.flush().await;
            }
        }
    }
}

async fn reader_task(
    read_half: tokio::net::unix::OwnedReadHalf,
    inner: Arc<MpvInner>,
    pending: PendingReplies,
) {
    let mut reader = BufReader::new(read_half).lines();
    let mut last_tick_at: Option<Instant> = None;
    let mut eof_reached = false;

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(frame) = parse_frame(trimmed) else {
                    tracing::debug!(line = %trimmed, "mpv: unparseable IPC frame, ignoring");
                    continue;
                };
                match frame {
                    Frame::Response {
                        request_id,
                        response,
                    } => {
                        if let Some(reply) = pending.lock().await.remove(&request_id) {
                            let result = if response.error == "success" {
                                Ok(response.data.unwrap_or(Value::Null))
                            } else {
                                Err(PlayerError::Backend(response.error))
                            };
                            let _ = reply.send(result);
                        }
                    }
                    Frame::Event(ev) => {
                        handle_event(&inner, ev, &mut last_tick_at, &mut eof_reached).await;
                    }
                }
            }
            Ok(None) => {
                // Socket closed cleanly — fall through and emit Exit.
                let snap = {
                    let g = inner.snapshot.lock().await;
                    g.clone()
                };
                let _ = inner.events.send(PlayerEvent::Exit {
                    position_s: snap.position_s,
                    duration_s: snap.duration_s,
                    reached_eof: eof_reached,
                });
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "mpv: IPC read failed");
                let _ = inner.events.send(PlayerEvent::Error {
                    message: format!("mpv IPC read: {e}"),
                });
                break;
            }
        }
    }
}

async fn handle_event(
    inner: &Arc<MpvInner>,
    ev: IpcEvent,
    last_tick_at: &mut Option<Instant>,
    eof_reached: &mut bool,
) {
    match ev {
        IpcEvent::PropertyChange { name, value } => {
            handle_property_change(inner, &name, &value, last_tick_at).await;
        }
        IpcEvent::EndFile { reason, error } => {
            *eof_reached = reason == "eof";
            if reason == "error" {
                let msg = error.unwrap_or_else(|| "end-file: unspecified".to_string());
                {
                    let mut snap = inner.snapshot.lock().await;
                    snap.state = PlayerState::Error;
                }
                let _ = inner.events.send(PlayerEvent::Error { message: msg });
            } else if *eof_reached {
                let mut snap = inner.snapshot.lock().await;
                snap.state = PlayerState::Ended;
                let _ = inner.events.send(PlayerEvent::state(PlayerState::Ended));
            }
        }
        IpcEvent::Shutdown => {
            let snap = {
                let g = inner.snapshot.lock().await;
                g.clone()
            };
            let _ = inner.events.send(PlayerEvent::Exit {
                position_s: snap.position_s,
                duration_s: snap.duration_s,
                reached_eof: *eof_reached,
            });
        }
        IpcEvent::Other { .. } => {}
    }
}

async fn handle_property_change(
    inner: &Arc<MpvInner>,
    name: &str,
    value: &Value,
    last_tick_at: &mut Option<Instant>,
) {
    match name {
        "time-pos" => {
            if let Some(pos) = value.as_f64() {
                let (snap_state, snap_paused, snap_duration) = {
                    let mut snap = inner.snapshot.lock().await;
                    snap.position_s = pos;
                    if snap.state == PlayerState::Loading {
                        snap.state = if snap.paused {
                            PlayerState::Paused
                        } else {
                            PlayerState::Playing
                        };
                    }
                    (snap.state, snap.paused, snap.duration_s)
                };
                let should_emit = match last_tick_at {
                    Some(t) => {
                        t.elapsed().as_secs_f64() >= PLAYER_POSITION_INTERVAL_S
                            || snap_state == PlayerState::Loading
                    }
                    None => true,
                };
                if should_emit {
                    *last_tick_at = Some(Instant::now());
                    let _ = inner.events.send(PlayerEvent::position(PositionTick {
                        position_s: pos,
                        duration_s: snap_duration,
                        paused: snap_paused,
                    }));
                }
                // Emit the state event the first time we move out of
                // Loading.
                if snap_state == PlayerState::Playing || snap_state == PlayerState::Paused {
                    let _ = inner.events.send(PlayerEvent::state(snap_state));
                }
            }
        }
        "duration" => {
            if let Some(d) = value.as_f64() {
                let mut snap = inner.snapshot.lock().await;
                snap.duration_s = d;
            }
        }
        "pause" => {
            let paused = value.as_bool().unwrap_or(false);
            let new_state = {
                let mut snap = inner.snapshot.lock().await;
                snap.paused = paused;
                if matches!(snap.state, PlayerState::Playing | PlayerState::Paused) {
                    snap.state = if paused {
                        PlayerState::Paused
                    } else {
                        PlayerState::Playing
                    };
                    Some(snap.state)
                } else {
                    None
                }
            };
            if let Some(s) = new_state {
                let _ = inner.events.send(PlayerEvent::state(s));
            }
            // Emit an immediate position tick on pause / resume so the
            // F-012 CW writer captures the exact transition point.
            let snap = { inner.snapshot.lock().await.clone() };
            *last_tick_at = Some(Instant::now());
            let _ = inner.events.send(PlayerEvent::position(PositionTick {
                position_s: snap.position_s,
                duration_s: snap.duration_s,
                paused,
            }));
        }
        "paused-for-cache" => {
            let stalled = value.as_bool().unwrap_or(false);
            let mut snap = inner.snapshot.lock().await;
            let new_state = if stalled {
                PlayerState::Buffering
            } else if snap.paused {
                PlayerState::Paused
            } else {
                PlayerState::Playing
            };
            if snap.state != new_state && snap.state.has_media() {
                snap.state = new_state;
                drop(snap);
                let _ = inner.events.send(PlayerEvent::state(new_state));
            }
        }
        "track-list" => {
            let parsed = TrackList::from_mpv_tracks(value);
            {
                let mut g = inner.tracks.lock().await;
                *g = parsed.clone();
            }
            let _ = inner.events.send(PlayerEvent::tracks(parsed));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_socket_path_uses_temp_dir_and_unique_filename() {
        let a = mint_socket_path();
        let b = mint_socket_path();
        assert_ne!(a, b);
        assert!(a.starts_with(std::env::temp_dir()));
        assert!(a
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with("kino-mpv-")));
    }

    #[test]
    fn mpv_binary_defaults_to_path_lookup_when_env_unset() {
        // We can't call `std::env::set_var` from a `unsafe_code = "forbid"`
        // crate to drive the override path, but we can assert the
        // default branch yields the bare binary name (which `Command`
        // resolves via `$PATH`).
        if std::env::var_os("KINO_MPV_PATH").is_none() {
            assert_eq!(mpv_binary(), "mpv");
        }
    }

    #[tokio::test]
    async fn spawn_with_missing_binary_yields_spawn_error() {
        let err = MpvBuilder::default()
            .with_binary("/nonexistent/kino-mpv-test-binary")
            .spawn()
            .await
            .unwrap_err();
        match err {
            PlayerError::Spawn(_) => {}
            other => panic!("expected Spawn, got {other:?}"),
        }
    }
}
