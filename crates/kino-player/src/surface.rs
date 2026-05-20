//! GTK widget-tree surgery for F-015 Linux libmpv in-window GL rendering
//! (Session 036 spike for ADR-133 Route B).
//!
//! PRD §F-015 (locked) requires libmpv to render "into a GL surface owned
//! by the Tauri window" with controls "composited over the surface by the
//! browser". Session 035's route enumeration picked **Route B**: wrap the
//! existing `WebKitWebView` in a [`gtk::Overlay`] alongside a sibling
//! [`gtk::GLArea`], with the webview's background set transparent so the
//! `SolidJS` overlay controls composite over the GL surface underneath.
//!
//! Session 036 is the **GTK-injection spike** that ADR-133 splits off
//! ahead of the driver implementation: prove that the surgery on Tauri
//! 2.11.2's internal widget tree can be performed via the **public Tauri
//! / wry API surface**, without forking wry or upstreaming a new
//! accessor.
//!
//! ## Spike result (Session 036)
//!
//! Avenue **(i)** from ADR-133 — walk + reparent post-window-realisation —
//! is the chosen path. Tauri 2.11.2's public API is sufficient:
//!
//! - [`tauri::WebviewWindow::default_vbox()`] returns the `gtk::Box` the
//!   webview is packed into (the runtime's "default vbox", a child of the
//!   `GtkApplicationWindow`).
//! - [`tauri::WebviewWindow::with_webview()`] hands a
//!   [`tauri::webview::PlatformWebview`] to a closure that runs on the
//!   GTK main thread; on Linux `PlatformWebview::inner()` returns the
//!   underlying `webkit2gtk::WebView` widget.
//! - `webkit2gtk::WebView` `@extends gtk::Container, gtk::Widget`, so all
//!   the standard widget-manipulation traits (parent / unparent / pack)
//!   apply.
//!
//! Neither of the two harder fallback avenues from ADR-133 is required:
//! we do **not** need to upstream a `WebViewExtUnix::gtk_box()` accessor
//! to wry (avenue (iii)) because Tauri already exposes the vbox, and we
//! do **not** need to intercept `tauri::Builder::setup` / `on_window_event`
//! before wry packs the webview (avenue (ii)) because the post-creation
//! walk is well-defined once the closure fires.
//!
//! ## Resulting widget tree
//!
//! Before surgery (vanilla Tauri 2 desktop):
//!
//! ```text
//! gtk::ApplicationWindow
//! └── gtk::Box (default_vbox)
//!     └── webkit2gtk::WebView (controls + GL canvas all in one)
//! ```
//!
//! After surgery (Route B):
//!
//! ```text
//! gtk::ApplicationWindow
//! └── gtk::Box (default_vbox, unchanged)
//!     └── gtk::Overlay
//!         ├── gtk::GLArea         (z=0, video — libmpv render target)
//!         └── webkit2gtk::WebView (z=1, transparent — SolidJS controls
//!                                  composite over the GL area)
//! ```
//!
//! ## What this spike does and does NOT prove
//!
//! Proven by the spike (compile-time + opt-in runtime invocation gated
//! behind `KINO_LIBMPV_SURFACE_SPIKE=1`):
//!
//! - The reparenting API path is reachable from kino's code without
//!   forking wry — Tauri 2.11.2 publicly exposes every accessor needed.
//! - The widget surgery (`WebView` unparent → `Overlay` with `GLArea` +
//!   `WebView` → repack overlay into vbox) is a small, well-typed
//!   sequence (this module is ~80 LOC).
//! - The webview's `set_background_color(RGBA(0,0,0,0))` call is a
//!   one-liner once we hold the `webkit2gtk::WebView` reference.
//!
//! NOT proven by the spike (deferred to PRD §F-015 §6B-1 hardware
//! verification + Session 037's libmpv driver):
//!
//! - That a libmpv `RenderContext` actually targets the `GLArea`
//!   correctly — that requires the [`libmpv2`] dependency to land
//!   (Session 037 scope per ADR-133).
//! - That the SolidJS controls visually composite over the GL surface
//!   on real hardware — the spike runs headless (no display in CI), so
//!   the visual verification is a §6B-1 line.
//! - That signal handlers wry installed on the webview survive
//!   reparenting cleanly across resize / focus / map-unmap cycles —
//!   wry attaches handlers to the widget object (not the container),
//!   so they SHOULD follow the widget, but Session 037 will exercise
//!   this on real hardware.
//!
//! ## Trigger
//!
//! Two host-side call sites in `kino-app` invoke [`inject_overlay`],
//! gated by mutually-exclusive Cargo features:
//!
//! 1.  **`libmpv-inprocess` feature ON (Session 037, ADR-133 / ADR-135).**
//!     `kino_app_lib::setup_libmpv_inprocess` runs the surgery AND
//!     attaches a [`LibmpvPlayer`] driver to the freshly-injected
//!     `GLArea`. Production rollout path; flips on by default after
//!     §6B-1 hardware verification (Session 038 deferred item (c)).
//! 2.  **`libmpv-surface-spike` feature ON + `KINO_LIBMPV_SURFACE_SPIKE=1`
//!     env var (Session 036 → Session 042, ADR-137).** Developer
//!     affordance: runs the surgery STANDALONE (no driver attach) so a
//!     developer can verify the widget-tree reparenting on their Linux
//!     distro WITHOUT installing `libmpv-dev`. Mutually exclusive with
//!     `libmpv-inprocess` (the cfg predicate excludes the spike call
//!     when inprocess is on, preventing a double-surgery error). The
//!     subprocess `MpvPlayer` (ADR-108) keeps driving playback when the
//!     spike runs, so the app remains operational.
//!
//! Default builds compile out both call sites entirely. CI exercises
//! the inprocess feature via the `lint` / `test` matrix (Session 038);
//! the surface-spike feature is not in the matrix because its surface
//! is small (one env-var check + a delegate call into `inject_overlay`)
//! and ADR-137 explains the trade-off.
//!
//! [`libmpv2`]: https://crates.io/crates/libmpv2
//! [`LibmpvPlayer`]: ../libmpv/struct.LibmpvPlayer.html

#![cfg(target_os = "linux")]

use gtk::prelude::*;
use webkit2gtk::WebViewExt;

/// Errors produced by [`inject_overlay`] when the widget tree doesn't
/// match the structure Tauri 2.11.2 builds at window-realisation time.
///
/// These are diagnostic — the spike is intentionally fail-loud rather
/// than fail-silent so a regression in Tauri's window scaffolding shows
/// up immediately in the log instead of producing a silently-broken
/// player surface.
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// The `WebKitWebView` is not yet parented when the closure fires.
    /// Should not happen — Tauri's `with_webview` posts onto the main
    /// thread after the webview is constructed and packed into the
    /// default vbox — but kept as a guard for future Tauri rewrites.
    #[error("webview has no parent yet (window not realised)")]
    NoParent,
    /// The parent widget is not a `gtk::Box`. Tauri 2.11.2 packs the
    /// webview into the runtime's "default vbox" (`gtk::Box`); a future
    /// Tauri may change this. The variant carries the offending widget
    /// type name for log readers.
    #[error("expected webview parent to be gtk::Box; got {0}")]
    UnexpectedParent(String),
}

/// Handle returned by [`inject_overlay`] holding the freshly-constructed
/// overlay structure.
///
/// Session 037 stores this in app-managed state so the libmpv driver can
/// resolve the GL area to bind its `RenderContext` against. Session 036
/// just logs the handle for verification.
#[derive(Debug, Clone)]
pub struct OverlaySurgery {
    /// The empty `GtkGLArea` libmpv will eventually render into.
    pub gl_area: gtk::GLArea,
    /// The `GtkOverlay` containing the GL area (z=0) and the webview
    /// (z=1, transparent).
    pub overlay: gtk::Overlay,
}

/// Reparent the Tauri webview into a `GtkOverlay` whose other child is
/// a `GtkGLArea` sized to fill the overlay.
///
/// Must run on the GTK main thread (the closure body of
/// [`tauri::WebviewWindow::with_webview`] is the canonical call site).
/// Returns the freshly-built `(gl_area, overlay)` pair on success.
///
/// # Errors
///
/// - [`SurfaceError::NoParent`] if the webview isn't yet parented (Tauri
///   regression — the closure should not fire until the widget is
///   realised).
/// - [`SurfaceError::UnexpectedParent`] if the webview's parent is not
///   a `gtk::Box` (Tauri rewrote its window scaffolding to use a
///   different container type — Session 037 + the spike both need to be
///   updated).
pub fn inject_overlay(webview: &webkit2gtk::WebView) -> Result<OverlaySurgery, SurfaceError> {
    // (1) Walk: find the gtk::Box the webview lives in. Tauri 2.11.2
    // packs the webview into `default_vbox` (a gtk::Box) via wry's
    // `WebViewBuilder::build_gtk(vbox)`. The walk is webview → parent
    // → downcast.
    let parent = webview.parent().ok_or(SurfaceError::NoParent)?;
    let vbox: gtk::Box = parent.downcast().map_err(|w| {
        // glib's downcast::<T>(self) returns Result<T, Self> — the Err
        // arm hands back the original widget so we can report its type.
        SurfaceError::UnexpectedParent(w.type_().name().to_string())
    })?;

    // (2) Reparent: detach the webview from the vbox. The widget is
    // GObject-refcounted; wry's internal reference keeps it alive
    // across the unparent / re-add cycle.
    vbox.remove(webview);

    // (3) Build the overlay: GtkOverlay with the GL area as the base
    // child (z=0) and the webview as an overlay child (z=1). The GL
    // area starts un-rendered (no GL context attached yet); Session
    // 037's libmpv driver will bind to it via `connect_render`.
    let overlay = gtk::Overlay::new();
    let gl_area = gtk::GLArea::new();
    // `set_has_alpha(false)` — the GL area itself is opaque; the
    // transparency lives in the OVERLAY child (the webview). PRD
    // §F-015 C3 ("composited over the surface by the browser") is
    // about the webview-over-GL composition direction.
    gl_area.set_has_alpha(false);
    // `set_auto_render(false)` — libmpv drives `queue_render` from its
    // render callback; we don't want GTK to also redraw on every
    // exposure. The render callback wiring lands in Session 037.
    gl_area.set_auto_render(false);
    overlay.add(&gl_area);
    overlay.add_overlay(webview);

    // (4) Webview transparency: PRD §F-015 C3 requires the webview to
    // render OVER the GL area with transparent regions revealing the
    // video. WebKit2GTK has supported transparent backgrounds since
    // 2.16; libwebkit2gtk-4.1 (Ubuntu 24.04, CI runner target) is well
    // past that.
    webview.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));

    // (5) Repack the overlay into the vbox. We use `pack_start` with
    // the same `(expand=true, fill=true, padding=0)` arguments wry
    // uses for the webview itself (see wry's `WebViewExtUnix::new_gtk`
    // doc) so the overlay fills the available space identically.
    vbox.pack_start(&overlay, true, true, 0);

    // (6) Show all newly-constructed widgets. The webview was already
    // visible; the overlay + GL area need their initial visibility
    // flag set.
    overlay.show_all();

    Ok(OverlaySurgery { gl_area, overlay })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_error_display_includes_parent_type() {
        let e = SurfaceError::UnexpectedParent("GtkBox".to_string());
        let s = format!("{e}");
        assert!(
            s.contains("gtk::Box"),
            "wording should mention expected type"
        );
        assert!(s.contains("GtkBox"), "wording should echo actual type");
    }

    #[test]
    fn surface_error_no_parent_display() {
        let e = SurfaceError::NoParent;
        let s = format!("{e}");
        assert!(s.contains("no parent"));
    }

    // Note: the surgery itself (`inject_overlay`) is not unit-tested
    // here because `gtk::init()` requires a display, which CI runners
    // and the agent's headless container lack. The runtime exercise
    // lives behind the `KINO_LIBMPV_SURFACE_SPIKE=1` env var (see
    // module docs) for human verification; structural compile-time
    // verification (this module's clippy pass) is what the spike's
    // §6A closure-path checkpoint hinges on.
}
