//! Linux in-process libmpv driver (PRD §F-015 + ADR-011 + ADR-133 Route
//! B + ADR-134 Session-036 spike → Session-037 driver).
//!
//! Implements the [`PlayerHandle`] trait by linking against the
//! [`libmpv2`] Rust binding and driving an embedded libmpv instance that
//! renders into the [`gtk::GLArea`] produced by [`crate::surface::
//! inject_overlay`]. The resulting widget tree (per [`crate::surface`]):
//!
//! ```text
//! gtk::ApplicationWindow
//! └── gtk::Box (default_vbox)
//!     └── gtk::Overlay
//!         ├── gtk::GLArea           (z=0, libmpv render target)
//!         └── webkit2gtk::WebView   (z=1, transparent, SolidJS overlay)
//! ```
//!
//! The driver runs entirely in the host process — no subprocess, no
//! separate window. PRD §F-015 §"Linux (locked architecture)" pins:
//!
//! 1. `libmpv-rs` in-process binding → satisfied by [`libmpv2`].
//! 2. GL surface owned by the Tauri window → satisfied by the
//!    [`gtk::GLArea`] sibling of the `WebKitWebView` (per ADR-134's
//!    Route B widget tree).
//! 3. Controls composited over the surface by the browser → satisfied
//!    by the webview's transparent background (`set_background_color
//!    (RGBA(0,0,0,0))`) and the SolidJS overlay route registered as
//!    z=1 of the `GtkOverlay`.
//!
//! Both the subprocess driver ([`crate::mpv::MpvPlayer`], ADR-108) and
//! this driver implement the same [`PlayerHandle`] trait; which one
//! ships at runtime is gated by the host's `libmpv-inprocess` Cargo
//! feature flag. ADR-133's multi-session split: Session 037 (this
//! file) lands the driver; Session 038 adds `libmpv2-dev` to CI's
//! apt-install list and `tauri.conf.json::bundle.linux.deb.depends`,
//! then flips the feature on by default after §6B-1 re-verification.
//!
//! ## Mpv config (PRD §F-015 Linux block)
//!
//! The mpv config the PRD locks (`crates/kino-server/assets/mpv.conf`)
//! is applied inline at initializer time via `set_property` calls —
//! this avoids the runtime path-resolution gymnastics that a
//! `load_config(path)` call would require (the asset lives inside the
//! Tauri bundle and isn't trivially addressable from a Rust crate
//! that doesn't know the bundle root). Properties applied:
//!
//! - `vo = libmpv` (mandatory for the render API — vanilla `vo=gpu`
//!   would open its own window).
//! - `hwdec = auto-safe`
//! - `keep-open = yes`
//! - `cache = yes`
//! - `demuxer-max-bytes = 200 MiB`
//! - `demuxer-readahead-secs = 20`
//! - `audio-spdif = ac3,dts,eac3,truehd,dts-hd`
//! - `sub-auto = fuzzy`
//! - `sub-ass = yes`
//! - `idle = yes` (the player stays alive between `loadfile`
//!   invocations — PRD §F-015 "open replaces existing").
//!
//! The PRD's `profile = high-quality` is applied after initialization
//! via `apply-profile high-quality` since mpv profiles aren't part of
//! the option set at initializer time.
//!
//! ## Event polling
//!
//! libmpv's event API is poll-based (`mpv_wait_event`). The driver
//! spawns a single OS thread that loops `wait_event(0.25s)` and
//! forwards each event to the broadcast channel + the snapshot/tracks
//! mutexes. PRD §8's `PLAYER_POSITION_INTERVAL_S = 5 s` cadence is
//! enforced by rate-limiting `Position` events on `time-pos` property
//! changes (mpv emits them as fast as the demuxer ticks; the driver
//! collapses to one per 5 s plus immediate ticks on seek / pause /
//! resume / EOF). Pattern matches the subprocess driver in
//! [`crate::mpv::MpvPlayer`].
//!
//! ## GL rendering loop
//!
//! 1. [`LibmpvPlayer::new_attached`] is called from the GTK main thread
//!    inside Tauri's `WebviewWindow::with_webview` closure, AFTER
//!    [`crate::surface::inject_overlay`] has produced the `GLArea`.
//! 2. The driver leaks the `Mpv` handle into `'static` so the
//!    `RenderContext<'static>` it owns can be stashed in the render
//!    signal handler.
//! 3. The driver creates the `RenderContext` with
//!    `RenderParam::ApiType(OpenGl)` + `RenderParam::InitParams(...)`.
//!    The `get_proc_address` callback uses `libc::dlsym(RTLD_DEFAULT,
//!    name)` — every GL symbol pulled in transitively by libwebkit2gtk
//!    (which links against libepoxy + libGL / libGLES / libEGL via
//!    GTK's GL stack) is resolvable through the process-wide symbol
//!    lookup. No link-time dependency on libepoxy-dev is needed.
//! 4. The driver connects the `GLArea::render` signal to a closure
//!    that calls `RenderContext::render(0, width, height, true)`. The
//!    FBO id `0` resolves to the framebuffer GTK has bound for the
//!    `GLArea` at render time (per GTK 3's `GtkGLArea` contract).
//! 5. The driver registers `RenderContext::set_update_callback` with a
//!    closure that posts `GLArea::queue_render` onto the GTK main loop
//!    via `glib::idle_add_once`. libmpv calls the update callback from
//!    its own thread; the `idle_add_once` indirection ensures the
//!    `queue_render` call always runs on the GTK main thread.
//!
//! ## Lifetime model
//!
//! One [`LibmpvPlayer`] is constructed at app startup (during the
//! Tauri setup() hook, after `inject_overlay`) and lives for the
//! lifetime of the process. `player_open` issues `loadfile`
//! (replaces the active media); `player_close` issues `stop`
//! (returns to idle). This differs from the subprocess driver
//! ([`crate::mpv::MpvPlayer`]) which spawns a fresh `mpv` process per
//! session because each subprocess opens its own window; the
//! in-process driver has no per-session window allocation cost.
//!
//! ## Thread safety notes
//!
//! - `libmpv2::Mpv` declares `unsafe impl Send + Sync` so commands +
//!   property reads/writes can be issued from any thread. The driver
//!   stashes the `Mpv` reference as a `'static` handle behind an
//!   `Arc<LibmpvInner>` cloned to each `PlayerHandle::*` invocation
//!   site.
//! - `libmpv2::render::RenderContext` does NOT implement `Send` (it
//!   carries a `PhantomData<&'a Mpv>` marker). The render context
//!   lives inside a `Rc<RefCell<...>>` captured by the
//!   `GLArea::connect_render` signal closure, which only fires on the
//!   GTK main thread.
//! - `gtk::GLArea` is `!Send` (all GTK widgets are thread-local). The
//!   driver never holds a `GLArea` reference after `new_attached`
//!   returns — only the render-signal closure (main-thread-pinned)
//!   does.

#![cfg(all(target_os = "linux", feature = "libmpv-inprocess"))]
// The libmpv driver consumes the libmpv2 crate's safe API except for
// one FFI call: `libc::dlsym(RTLD_DEFAULT, ...)` for libmpv's OpenGL
// function pointer resolution. Per ADR-135 this is the single scoped
// exception to kino-player's crate-level `unsafe_code = "deny"`.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use async_trait::async_trait;
use gtk::prelude::*;
use libmpv2::events::{Event as MpvEvent, PropertyData};
use libmpv2::render::{OpenGLInitParams, RenderParam, RenderParamApiType};
use libmpv2::{Format, Mpv};
use tokio::sync::broadcast;

use crate::error::PlayerError;
use crate::event::{PlayerEvent, PositionTick};
use crate::handle::{OpenRequest, PlayerHandle};
use crate::state::{PlayerSnapshot, PlayerState};
use crate::surface::OverlaySurgery;
use crate::tracks::TrackList;

/// PRD §8 `PLAYER_POSITION_INTERVAL_S = 5 s`. Duplicated from
/// [`crate::mpv`] for the same reason it's duplicated there: keeping
/// `kino-player` cycle-free from the `kino-core::constants` crate.
const PLAYER_POSITION_INTERVAL_S: f64 = 5.0;

/// Event channel capacity. Matches the subprocess driver
/// ([`crate::mpv::MpvPlayer`]).
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// libmpv `wait_event` timeout (seconds). Short enough that a shutdown
/// flag is observed promptly; long enough that the polling thread isn't
/// spinning.
const WAIT_EVENT_TIMEOUT_S: f64 = 0.25;

/// `RTLD_DEFAULT` handle for `dlsym` — process-wide symbol lookup. On
/// glibc/Linux this is `NULL` (the `libc` crate doesn't expose the
/// constant for `x86_64-unknown-linux-gnu` so we declare it
/// explicitly). FreeBSD / macOS / Android use different sentinel
/// values; this module is `#[cfg(target_os = "linux")]`-only so the
/// glibc semantics apply.
const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

/// `get_proc_address` callback handed to libmpv's
/// [`OpenGLInitParams`]. Delegates to `dlsym(RTLD_DEFAULT, name)`
/// which performs a process-wide symbol lookup — every GL symbol
/// pulled in transitively by libwebkit2gtk's libepoxy linkage is
/// resolvable through this path on Linux glibc.
///
/// libmpv calls this for every GL function it wants to resolve at
/// init time; the callback returns a function pointer or NULL. NULL
/// returns are libmpv-friendly (it falls back internally or skips the
/// optional extension).
fn dlsym_proc_address(_: &(), name: &str) -> *mut c_void {
    // CString allocation per resolution is cheap (a few dozen names
    // total, called once during render-context construction).
    let Ok(cstr) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `dlsym(RTLD_DEFAULT, ...)` is a pure symbol lookup over
    // the process's already-loaded shared libraries. It's
    // re-entrant-safe and thread-safe per POSIX. The returned pointer
    // is either NULL (symbol not found) or a function pointer libmpv
    // is contractually allowed to call back into.
    unsafe { libc::dlsym(RTLD_DEFAULT, cstr.as_ptr()) }
}

/// Driver implementing [`PlayerHandle`] on top of an embedded libmpv
/// instance.
///
/// Built once at app startup via [`LibmpvPlayer::new_attached`]; lives
/// for the lifetime of the process. The Tauri command handlers hold
/// `Arc<dyn PlayerHandle>` clones of the driver and call into it
/// from any thread (the `Mpv` handle is `Send + Sync`).
#[derive(Clone)]
pub struct LibmpvPlayer {
    inner: Arc<LibmpvInner>,
}

struct LibmpvInner {
    /// libmpv handle leaked into `'static`. The leak is deliberate —
    /// the driver lives for the process lifetime; recovering the
    /// memory would race the GTK main thread's render-signal handler.
    mpv: &'static Mpv,
    /// Broadcast of [`PlayerEvent`] to all subscribers.
    events: broadcast::Sender<PlayerEvent>,
    /// Latest snapshot; updated by the polling thread.
    snapshot: StdMutex<PlayerSnapshot>,
    /// Latest track list; updated by the polling thread.
    tracks: StdMutex<TrackList>,
    /// `true` once the polling thread should exit. Set on `Drop`.
    shutdown: AtomicBool,
}

/// Generation counter for `observe_property` reply ids. The polling
/// thread routes events back via the property name (not the id), so
/// the actual id values are irrelevant — but `mpv_observe_property`
/// requires distinct ids per observation.
static NEXT_OBSERVE_ID: AtomicU64 = AtomicU64::new(1);

impl LibmpvPlayer {
    /// Construct the driver attached to a freshly-injected overlay.
    ///
    /// MUST be called from the GTK main thread (the canonical call
    /// site is inside the closure of
    /// [`tauri::WebviewWindow::with_webview`]). The function:
    ///
    /// 1. Creates a fresh [`Mpv`] instance with the PRD §F-015 Linux
    ///    config applied via `set_property` calls.
    /// 2. Applies the `high-quality` mpv profile via `apply-profile`.
    /// 3. Creates a [`RenderContext`] bound to OpenGL with the
    ///    epoxy-based `get_proc_address` callback.
    /// 4. Connects the [`gtk::GLArea::render`] signal to call
    ///    `RenderContext::render(0, w, h, true)`.
    /// 5. Registers `RenderContext::set_update_callback` to post a
    ///    `GLArea::queue_render` onto the GTK main loop via
    ///    `glib::idle_add_local_once`.
    /// 6. Spawns the event-polling thread.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Spawn`] wrapping the libmpv error if
    /// any of the initialization steps fail (out-of-memory, libmpv
    /// version mismatch, render context creation failure, etc.).
    #[allow(clippy::needless_pass_by_value)] // surgery is consumed; the GLArea clone moves into the render closure
    pub fn new_attached(surgery: OverlaySurgery) -> Result<Self, PlayerError> {
        // (1) Create Mpv with PRD §F-015 Linux config applied at init
        // time. The properties below mirror crates/kino-server/assets/
        // mpv.conf line-for-line; the resulting libmpv behavior is
        // bit-equivalent to running `mpv --config=mpv.conf`.
        let mpv = Mpv::with_initializer(|init| {
            // `vo=libmpv` is mandatory for the render API path. Without
            // it libmpv would attempt to open its own gpu window — the
            // exact behaviour ADR-133 Route B replaces.
            init.set_property("vo", "libmpv")?;
            init.set_property("hwdec", "auto-safe")?;
            init.set_property("keep-open", "yes")?;
            init.set_property("cache", "yes")?;
            // 200 MiB demuxer buffer (mpv.conf: `demuxer-max-bytes=200M`).
            init.set_property("demuxer-max-bytes", 200_i64 * 1024 * 1024)?;
            init.set_property("demuxer-readahead-secs", 20.0_f64)?;
            init.set_property("audio-spdif", "ac3,dts,eac3,truehd,dts-hd")?;
            init.set_property("sub-auto", "fuzzy")?;
            init.set_property("sub-ass", "yes")?;
            // `idle=yes` — without this, libmpv exits the playback
            // core after the last file ends; we need it to stay alive
            // between `loadfile` invocations so `player_open` /
            // `player_close` cycle on the same Mpv handle.
            init.set_property("idle", "yes")?;
            Ok(())
        })
        .map_err(|e| map_libmpv_err(&e))?;

        // (1.5) `profile=high-quality` from mpv.conf. mpv profiles
        // aren't part of the option set at init time; they're applied
        // post-init via the `apply-profile` command. Errors here are
        // non-fatal — if libmpv was built without profile support
        // (very unusual) the player still works with the inline
        // properties above.
        if let Err(e) = mpv.command("apply-profile", &["high-quality"]) {
            tracing::warn!(
                error = %e,
                "libmpv: apply-profile high-quality failed; continuing with inline properties"
            );
        }

        // (2) Leak into 'static so the RenderContext we build below
        // can be stashed in the render-signal closure (which outlives
        // any non-'static borrow).
        let mpv: &'static Mpv = Box::leak(Box::new(mpv));

        // (3) Observe the properties the driver translates into
        // PlayerEvents. IDs are arbitrary unique integers — the
        // polling thread dispatches on the property NAME (the libmpv
        // event carries it).
        observe(mpv, "time-pos", Format::Double)?;
        observe(mpv, "duration", Format::Double)?;
        observe(mpv, "pause", Format::Flag)?;
        observe(mpv, "paused-for-cache", Format::Flag)?;
        observe(mpv, "eof-reached", Format::Flag)?;
        // track-list is a Node-typed property; libmpv2 6.0.0's
        // PropertyData doesn't expose Node, so we observe a scalar
        // (e.g. track-list/count) as a trigger and re-fetch the
        // full list as a JSON string when the count changes.
        observe(mpv, "track-list/count", Format::Int64)?;

        // (4) Render context. The GLContext type parameter is `()`
        // because the get_proc_address callback ignores its `ctx`
        // argument — libepoxy's symbol lookup is global.
        let render_context = mpv
            .create_render_context(vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(OpenGLInitParams {
                    get_proc_address: dlsym_proc_address,
                    ctx: (),
                }),
            ])
            .map_err(|e| map_libmpv_err(&e))?;

        // (5) Wire the GLArea::render signal. The render context lives
        // inside a Rc<RefCell<...>> shared between the render-signal
        // closure (immutable borrow during render) and the
        // update-callback indirection (we re-borrow to call
        // RenderContext::set_update_callback below).
        let render_context = Rc::new(RefCell::new(render_context));
        let gl_area = surgery.gl_area.clone();
        let rc_for_render = Rc::clone(&render_context);
        gl_area.connect_render(move |area, _gl_ctx| {
            let scale = area.scale_factor();
            // `allocated_width/height` are in GTK logical pixels;
            // multiply by scale_factor to get device pixels (HiDPI).
            let width = area.allocated_width() * scale;
            let height = area.allocated_height() * scale;
            // FBO id 0 — GTK's GLArea binds its own framebuffer
            // before firing the render signal; from the application's
            // GL state perspective that's the "default" target. The
            // `flip = true` argument tells libmpv to render with the
            // Y axis flipped (mpv expects positive-Y-up; GTK gives us
            // positive-Y-down).
            if let Err(e) =
                rc_for_render
                    .borrow()
                    .render::<()>(0, width.max(1), height.max(1), true)
            {
                tracing::warn!(error = %e, "libmpv: render call failed");
            }
            glib::Propagation::Stop
        });

        // (6) Update callback: libmpv asks for a redraw from its
        // render thread. We can't `queue_render` directly from
        // there (GTK requires main-thread access AND `gtk::GLArea`
        // is `!Send` so even `glib::idle_add_once` — which takes
        // `Send` closures — can't capture a weak reference). The
        // dispatch goes through an `async-channel`:
        //
        // - `tx` (Send + Sync) is captured by the libmpv update
        //   callback and pinged on every frame.
        // - A `MainContext::spawn_local` future on the GTK main
        //   thread holds the (!Send) `GLArea` clone and drains the
        //   receiver, calling `queue_render()` for each ping.
        //
        // `spawn_local` accepts `!Send` futures because the local
        // main context never moves work between threads.
        let (update_tx, update_rx) = async_channel::unbounded::<()>();
        let area_for_redraw = gl_area.clone();
        glib::MainContext::default().spawn_local(async move {
            while update_rx.recv().await.is_ok() {
                area_for_redraw.queue_render();
            }
        });
        render_context.borrow_mut().set_update_callback(move || {
            // try_send is non-blocking — if the receiver hasn't
            // drained the previous ping yet, the redraw is already
            // pending. The channel is unbounded so try_send only
            // fails if the receiver was dropped, which here means
            // the app is shutting down.
            let _ = update_tx.try_send(());
        });

        // (7) Construct the shared inner + spawn the polling thread.
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let inner = Arc::new(LibmpvInner {
            mpv,
            events: events_tx,
            snapshot: StdMutex::new(PlayerSnapshot::idle(String::new())),
            tracks: StdMutex::new(TrackList::default()),
            shutdown: AtomicBool::new(false),
        });

        let inner_for_thread = Arc::clone(&inner);
        // Note on RenderContext lifetime: the `Rc<RefCell<RenderContext>>`
        // is reference-counted, with two strong refs alive — `rc_for_render`
        // captured by the GLArea render-signal closure, and the local
        // `render_context` handle that drops at the end of this function.
        // After this function returns, the closure's Rc is the sole
        // strong ref keeping the RenderContext alive — which matches the
        // lifetime of the GLArea + signal binding (i.e. process
        // lifetime, since the GLArea is in the leaked overlay surgery).

        std::thread::Builder::new()
            .name("kino-libmpv-events".to_string())
            .spawn(move || event_loop(inner_for_thread))
            .map_err(|e| {
                PlayerError::Spawn(std::io::Error::other(format!("libmpv event thread: {e}")))
            })?;

        Ok(Self { inner })
    }
}

/// Helper: observe a property with a freshly-minted id. The id is
/// unused in the dispatch path (we key on property name) but mpv
/// requires it be distinct per observation.
fn observe(mpv: &Mpv, name: &str, format: Format) -> Result<(), PlayerError> {
    let id = NEXT_OBSERVE_ID.fetch_add(1, Ordering::Relaxed);
    mpv.observe_property(name, format, id)
        .map_err(|e| map_libmpv_err(&e))
}

/// Convert a libmpv2 error to a [`PlayerError::Spawn`] wrapping a
/// best-effort `io::Error` description. [`PlayerError`] doesn't have
/// a dedicated `Backend(libmpv2::Error)` variant — the existing
/// `Backend(String)` is too generic for the construction path's I/O
/// errors, so we route libmpv init failures through `Spawn` (matching
/// the subprocess driver's "couldn't start the backend" path).
fn map_libmpv_err(e: &libmpv2::Error) -> PlayerError {
    PlayerError::Spawn(std::io::Error::other(format!("libmpv: {e}")))
}

/// Convert a libmpv2 error to [`PlayerError::Backend`]. Used by the
/// command-issuing [`PlayerHandle`] methods where the spawn happened
/// long ago.
fn backend_err(e: &libmpv2::Error) -> PlayerError {
    PlayerError::Backend(format!("libmpv: {e}"))
}

#[async_trait]
impl PlayerHandle for LibmpvPlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        self.inner
            .snapshot
            .lock()
            .map_or_else(|_| PlayerSnapshot::idle(String::new()), |g| g.clone())
    }

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.inner.events.subscribe()
    }

    async fn open(&self, req: OpenRequest) -> Result<(), PlayerError> {
        // Mirror the subprocess driver's eager-snapshot pattern — the
        // host's first `snapshot()` after open() returns must already
        // reflect the new token even if the libmpv FileLoaded event
        // hasn't fired yet.
        {
            let mut snap = self
                .inner
                .snapshot
                .lock()
                .map_err(|_| PlayerError::Closed("snapshot mutex poisoned".to_string()))?;
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

        // `loadfile <url> replace [<index> <options>]`. mpv accepts a
        // comma-separated options string as the 4th positional. Pass
        // `start=<seconds>` to resume.
        let start_opt = if req.resume_position_s > 0.0 {
            format!("start={}", req.resume_position_s)
        } else {
            String::new()
        };
        let cmd_args: Vec<&str> = if start_opt.is_empty() {
            vec![&req.url, "replace"]
        } else {
            vec![&req.url, "replace", "0", &start_opt]
        };
        self.inner
            .mpv
            .command("loadfile", &cmd_args)
            .map_err(|e| backend_err(&e))
    }

    async fn close(&self) -> Result<(), PlayerError> {
        // Snapshot the current position BEFORE issuing stop so the
        // Exit event we synthesise reports the last position the
        // polling thread observed, not the post-stop zeroed value.
        let snap = self.snapshot();

        // `stop` clears the active playback but keeps the player
        // alive (because we set `idle=yes` at init). The polling
        // thread will see an EndFile event with reason=Stop and
        // emit its own Exit; the eager Exit below makes the host's
        // `player_close` return synchronously without racing the
        // polling thread.
        if let Err(e) = self.inner.mpv.command("stop", &[]) {
            tracing::warn!(error = %e, "libmpv: stop command failed");
        }

        let _ = self.inner.events.send(PlayerEvent::Exit {
            position_s: snap.position_s,
            duration_s: snap.duration_s,
            reached_eof: false,
        });
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.inner
            .mpv
            .set_property("pause", paused)
            .map_err(|e| backend_err(&e))
    }

    async fn seek(&self, position_s: f64) -> Result<(), PlayerError> {
        // libmpv seek command: `seek <amount> [<flags>]`. Use
        // `absolute+exact` for frame-accurate seeking — matches the
        // subprocess driver.
        let pos_str = position_s.to_string();
        self.inner
            .mpv
            .command("seek", &[&pos_str, "absolute", "exact"])
            .map_err(|e| backend_err(&e))
    }

    async fn select_audio_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        match track_id {
            Some(id) => self
                .inner
                .mpv
                .set_property("aid", id.to_string())
                .map_err(|e| backend_err(&e)),
            None => self
                .inner
                .mpv
                .set_property("aid", "no")
                .map_err(|e| backend_err(&e)),
        }
    }

    async fn select_subtitle_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        match track_id {
            Some(id) => self
                .inner
                .mpv
                .set_property("sid", id.to_string())
                .map_err(|e| backend_err(&e)),
            None => self
                .inner
                .mpv
                .set_property("sid", "no")
                .map_err(|e| backend_err(&e)),
        }
    }

    fn tracks(&self) -> TrackList {
        self.inner
            .tracks
            .lock()
            .map_or_else(|_| TrackList::default(), |g| g.clone())
    }
}

// ---- event-polling thread ----------------------------------------------

/// Drains libmpv events on a dedicated OS thread. The loop exits when
/// `inner.shutdown` is set or when a Shutdown event arrives from
/// libmpv (which shouldn't happen during normal operation because the
/// player is kept idle between sessions).
#[allow(clippy::needless_pass_by_value)] // owned Arc lives for the thread's lifetime
fn event_loop(inner: Arc<LibmpvInner>) {
    let mut last_tick_at: Option<Instant> = None;
    let mut eof_reached = false;
    while !inner.shutdown.load(Ordering::Acquire) {
        match inner.mpv.wait_event(WAIT_EVENT_TIMEOUT_S) {
            Some(Ok(event)) => {
                if let MpvEvent::Shutdown = event {
                    // Synthesise a final Exit event before exiting
                    // the loop; subsequent receivers see the channel
                    // close cleanly.
                    let snap = read_snapshot(&inner);
                    let _ = inner.events.send(PlayerEvent::Exit {
                        position_s: snap.position_s,
                        duration_s: snap.duration_s,
                        reached_eof: eof_reached,
                    });
                    break;
                }
                handle_mpv_event(&inner, event, &mut last_tick_at, &mut eof_reached);
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "libmpv: wait_event returned error");
            }
            None => {} // timeout — loop checks shutdown flag
        }
    }
}

fn read_snapshot(inner: &LibmpvInner) -> PlayerSnapshot {
    inner
        .snapshot
        .lock()
        .map_or_else(|_| PlayerSnapshot::idle(String::new()), |g| g.clone())
}

fn handle_mpv_event(
    inner: &LibmpvInner,
    event: MpvEvent<'_>,
    last_tick_at: &mut Option<Instant>,
    eof_reached: &mut bool,
) {
    match event {
        MpvEvent::PropertyChange { name, change, .. } => {
            handle_property_change(inner, name, &change, last_tick_at);
        }
        MpvEvent::EndFile(reason) => {
            // EndFileReason values: Eof / Stop / Quit / Error / Redirect.
            // Map per the subprocess driver:
            // - Eof   → Ended state + emit state event
            // - Error → Error state + emit Error event (the message
            //           is not exposed by libmpv2 6.0.0 beyond the
            //           reason code; we send the reason discriminant)
            // - other → no terminal emission (Stop / Quit happen on
            //           our own close() path, where we synthesise the
            //           Exit ourselves)
            if reason == libmpv2::mpv_end_file_reason::Eof {
                *eof_reached = true;
                if let Ok(mut snap) = inner.snapshot.lock() {
                    snap.state = PlayerState::Ended;
                }
                let _ = inner.events.send(PlayerEvent::state(PlayerState::Ended));
            } else if reason == libmpv2::mpv_end_file_reason::Error {
                if let Ok(mut snap) = inner.snapshot.lock() {
                    snap.state = PlayerState::Error;
                }
                let _ = inner.events.send(PlayerEvent::Error {
                    message: format!("libmpv end-file reason: {reason}"),
                });
            }
        }
        MpvEvent::FileLoaded => {
            // First successful demux. Refresh the track list now —
            // observers will fire again as additional tracks load.
            refresh_track_list(inner);
        }
        // Other events (StartFile / Seek / PlaybackRestart /
        // VideoReconfig / AudioReconfig / etc.) don't currently map
        // to PlayerEvents — they're informational. PlaybackRestart
        // would be a candidate trigger for the resume-seek
        // synchronization point if we ever needed it; the loadfile
        // `start=` option already covers PRD §F-015's resume-position
        // behavior, so the explicit handler is unnecessary today.
        _ => {}
    }
}

fn handle_property_change(
    inner: &LibmpvInner,
    name: &str,
    change: &PropertyData<'_>,
    last_tick_at: &mut Option<Instant>,
) {
    match name {
        "time-pos" => {
            if let PropertyData::Double(pos) = change {
                let (snap_state, snap_paused, snap_duration) = {
                    let Ok(mut snap) = inner.snapshot.lock() else {
                        return;
                    };
                    snap.position_s = *pos;
                    if snap.state == PlayerState::Loading {
                        snap.state = if snap.paused {
                            PlayerState::Paused
                        } else {
                            PlayerState::Playing
                        };
                    }
                    (snap.state, snap.paused, snap.duration_s)
                };
                let should_emit = last_tick_at
                    .is_none_or(|t| t.elapsed().as_secs_f64() >= PLAYER_POSITION_INTERVAL_S);
                if should_emit {
                    *last_tick_at = Some(Instant::now());
                    let _ = inner.events.send(PlayerEvent::position(PositionTick {
                        position_s: *pos,
                        duration_s: snap_duration,
                        paused: snap_paused,
                    }));
                }
                if snap_state == PlayerState::Playing || snap_state == PlayerState::Paused {
                    let _ = inner.events.send(PlayerEvent::state(snap_state));
                }
            }
        }
        "duration" => {
            if let PropertyData::Double(d) = change {
                if let Ok(mut snap) = inner.snapshot.lock() {
                    snap.duration_s = *d;
                }
            }
        }
        "pause" => {
            if let PropertyData::Flag(paused) = change {
                let new_state = {
                    let Ok(mut snap) = inner.snapshot.lock() else {
                        return;
                    };
                    snap.paused = *paused;
                    if matches!(snap.state, PlayerState::Playing | PlayerState::Paused) {
                        snap.state = if *paused {
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
                // Immediate position tick on pause/resume so F-012 CW
                // captures the exact transition point (matches the
                // subprocess driver).
                let snap = read_snapshot(inner);
                *last_tick_at = Some(Instant::now());
                let _ = inner.events.send(PlayerEvent::position(PositionTick {
                    position_s: snap.position_s,
                    duration_s: snap.duration_s,
                    paused: *paused,
                }));
            }
        }
        "paused-for-cache" => {
            if let PropertyData::Flag(stalled) = change {
                if let Ok(mut snap) = inner.snapshot.lock() {
                    let new_state = if *stalled {
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
            }
        }
        "track-list/count" => {
            // Scalar trigger; the real list is fetched as a JSON
            // string and parsed via the existing TrackList::
            // from_mpv_tracks helper that the subprocess driver also
            // uses.
            refresh_track_list(inner);
        }
        _ => {}
    }
}

/// Fetch the libmpv `track-list` property as a JSON string, parse it
/// into a [`TrackList`], stash it in `inner.tracks`, and broadcast a
/// `Tracks` event.
fn refresh_track_list(inner: &LibmpvInner) {
    // libmpv exposes Node-typed properties as JSON when read via
    // STRING format. The subprocess driver consumes the same JSON
    // shape via the `track-list` event; this keeps the parser path
    // identical between drivers.
    let json: String = match inner.mpv.get_property("track-list") {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "libmpv: track-list property read failed");
            return;
        }
    };
    let parsed = match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(v) => TrackList::from_mpv_tracks(&v),
        Err(e) => {
            tracing::debug!(error = %e, "libmpv: track-list JSON parse failed");
            return;
        }
    };
    if let Ok(mut g) = inner.tracks.lock() {
        *g = parsed.clone();
    }
    let _ = inner.events.send(PlayerEvent::tracks(parsed));
}

impl Drop for LibmpvInner {
    fn drop(&mut self) {
        // Signal the polling thread to exit. We don't join here —
        // the thread will see the flag within WAIT_EVENT_TIMEOUT_S
        // and exit on its own. Joining would require holding a
        // JoinHandle which would force a non-Clone field on
        // LibmpvPlayer; since the player is process-lifetime anyway
        // (cf. the leaked Mpv), best-effort shutdown is fine.
        self.shutdown.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_libmpv_err_wraps_in_spawn_variant() {
        let e = libmpv2::Error::Null;
        let mapped = map_libmpv_err(&e);
        assert!(matches!(mapped, PlayerError::Spawn(_)));
        let msg = mapped.to_string();
        assert!(msg.contains("libmpv"));
    }

    #[test]
    fn backend_err_wraps_in_backend_variant() {
        let e = libmpv2::Error::Null;
        let mapped = backend_err(&e);
        match mapped {
            PlayerError::Backend(msg) => assert!(msg.contains("libmpv")),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn dlsym_proc_address_returns_null_for_garbage_name() {
        // The function should at least not panic on a name containing
        // a null byte (CString::new fails → return null).
        let p = dlsym_proc_address(&(), "garbage\0name");
        assert!(p.is_null());
    }

    // Note: the actual Mpv / RenderContext / GLArea wiring is not
    // unit-testable without a real GL context + GTK main loop. The
    // §6B-1 hardware-verification line (Linux AppImage launches +
    // libmpv plays a stream end-to-end) is the runtime acceptance
    // gate. Structural compile-time verification (this module's
    // clippy pass with `--features libmpv-inprocess`) is what the
    // §6A closure-path checkpoint hinges on.
}
