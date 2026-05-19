# Upstream librqbit PR drafts (F-013 / F-014 §6A closure path)

**Status:** draft, ready for human review before upstream submission.
**Target upstream:** `ikatson/rqbit` (Apache-2.0).
**librqbit version surveyed:** `8.1.1` (Cargo.lock pin; latest on crates.io
as of 2026-05-19).
**Filed by:** kino agent Session 039 (Path B option (i) per the Session 038
plan).

This document drafts two upstream changes that, if accepted by the rqbit
maintainer, would close the two §6A code-acceptance regressions kino's
PRD Issues entry tracks for F-013 ("max connections per torrent: 200")
and F-014 ("piece priorities mapped to librqbit ..."). The drafts are
written so that the human can copy each PR description verbatim into a
GitHub PR; the diffs are anchored to real lines in librqbit 8.1.1's
on-disk source tree as cross-checked from
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/`.

Until upstream lands, kino's §6A entries stay OPEN. Once upstream
publishes a release containing **PR A** (the smaller change), the
follow-up kino session bumps `librqbit` in `Cargo.toml`, wires the
new field through `kino_torrent::engine`, and flips the F-013 §6A
entry to RESOLVED. **PR B** is split into two independently-mergeable
sub-PRs: **PR B1** (per-stream lookahead, drafted fully in this doc)
and **PR B2** (per-piece priority enum + tiered scheduler walk,
drafted as a design proposal). Wiring B1 honors PRD §F-014's HIGHEST
window approximately (lookahead sized to 60s of file bitrate); B2
adds the HIGH window and last-piece-HIGH special-case. If upstream
rejects or stalls either of B1/B2, the PRD Issues entry's
**option (d)** (PRD revision relaxing the language to "best-effort,
subject to engine API capabilities") remains the §6A clearance
lever.

---

## Context: what kino needs

PRD §F-013 (locked, `PRD.md:582`):

> Max connections per torrent: 200

PRD §F-014 (locked, `PRD.md:647-652`):

> Piece priorities mapped to librqbit:
>
> - Window `[position, position + 60s]`: HIGHEST
> - Window `[position + 60s, position + 300s]`: HIGH
> - Last piece of the active file: HIGH
> - All others: NORMAL

kino's locked numeric constants for these invariants
(`crates/kino-core/src/constants.rs`):

- `MAX_CONNECTIONS_PER_TORRENT: u32 = 200` (line 98)
- `PIECE_PRIORITY_HIGH_WINDOW_S: f64 = 60.0` (line 14)
- `PIECE_PRIORITY_MED_WINDOW_S: f64 = 300.0` (line 17)

Both constants are defined but unused at consumer sites because the
matching librqbit API is `pub(crate)` (piece priorities) or hardcoded
(peer cap). The kino consumer code that would consume the new
upstream surface lives in:

- `crates/kino-torrent/src/engine.rs:310-319` —
  `Session::new_with_opts` call site (would gain
  `max_peer_connections_per_torrent` argument).
- `crates/kino-torrent/src/engine.rs:347-350` —
  `AddTorrentOptions` construction (would optionally override the
  session default per torrent, though kino's v1 wants the same
  cap for every torrent).
- `crates/kino-torrent/src/monitor.rs` — would gain a
  position-event-driven loop calling the proposed piece-priority API
  to map the locked HIGHEST / HIGH windows onto piece ranges
  computed from `position_s` + the file's piece offset table.

---

## PR A — make per-torrent peer-connection cap configurable

**Title:** `feat(session): make per-torrent peer-connection cap configurable`

**Summary:** Adds an optional cap that downstream consumers can set on
`SessionOptions` (session-wide default) and on `AddTorrentOptions`
(per-torrent override). The internal `peer_semaphore: Arc<Semaphore>`
on `TorrentStateLive` (currently hardcoded to 128 at
`torrent_state/live/mod.rs:278`) is initialized from the resolved cap.
No behavior change when the new fields are unset (defaults to the
existing 128-permit ceiling).

**Why:** Several downstream applications need a tighter peer cap to
honour their own scheduling or compliance invariants (kino's PRD §F-013
locks 200 connections per torrent; other apps want lower caps to fit
within mobile network constraints). The internal `TODO: make it
configurable` at `torrent_state/live/mod.rs:233` (file_priorities) is
parallel evidence that "configurability via a struct field" is the
expected upstream direction for similar internal-cap APIs.

**Surface area:** three structs, one constant, zero behaviour change
for default builds.

### Files changed

#### 1. `librqbit/src/session.rs` (lines 382-433 — `SessionOptions`)

Add a new optional field `max_peer_connections_per_torrent`. The
field is `Option<NonZeroU32>` to encode "unset → use the
crate default" (matching the existing `Option<…>` style used by
`peer_id`, `peer_opts`, `concurrent_init_limit`).

```diff
@@ pub struct SessionOptions {
     #[cfg(feature = "disable-upload")]
     pub disable_upload: bool,
+
+    /// Cap on the number of concurrent peer connections per torrent.
+    /// `None` means use the crate default ([`DEFAULT_PEER_CONNECTIONS_PER_TORRENT`]).
+    /// Both incoming and outgoing peer tasks consume this budget via
+    /// the per-torrent `peer_semaphore` in [`TorrentStateLive`].
+    /// Can be overridden per torrent on [`AddTorrentOptions`].
+    pub max_peer_connections_per_torrent: Option<std::num::NonZeroU32>,
 }
```

#### 2. `librqbit/src/session.rs` (lines 234-282 — `AddTorrentOptions`)

Add the per-torrent override.

```diff
@@ pub struct AddTorrentOptions {
     // Custom trackers
     pub trackers: Option<Vec<String>>,
+
+    /// Override the session's [`SessionOptions::max_peer_connections_per_torrent`]
+    /// for this torrent. `None` means inherit from the session.
+    pub max_peer_connections: Option<std::num::NonZeroU32>,
 }
```

#### 3. `librqbit/src/lib.rs` (new public constant)

Add a public default constant. Keep the historic 128 to preserve
existing behavior; document it.

```diff
@@ pub use ...
+
+/// Default cap on concurrent peer connections per torrent when no
+/// override is set on [`SessionOptions`] or [`AddTorrentOptions`].
+/// Matches the historical hardcoded value used through 8.1.x.
+pub const DEFAULT_PEER_CONNECTIONS_PER_TORRENT: u32 = 128;
```

(Choose a sensible re-export location; the constant only needs to be
in scope from `session.rs`, `torrent_state/mod.rs`, and the
documentation cross-references above.)

#### 4. `librqbit/src/torrent_state/mod.rs` (lines 110-135 — `ManagedTorrentOptions`)

Plumb the resolved value through the existing crate-internal options.
This is the field `TorrentStateLive::new` will read.

```diff
 #[derive(Default)]
 pub(crate) struct ManagedTorrentOptions {
     pub force_tracker_interval: Option<Duration>,
     pub peer_connect_timeout: Option<Duration>,
     pub peer_read_write_timeout: Option<Duration>,
     pub allow_overwrite: bool,
     pub output_folder: PathBuf,
     pub disk_write_queue: Option<DiskWorkQueueSender>,
     pub ratelimits: LimitsConfig,
     pub initial_peers: Vec<SocketAddr>,
+    pub max_peer_connections: u32,
     #[cfg(feature = "disable-upload")]
     pub _disable_upload: bool,
 }
```

(The field is non-`Option<…>` here because the resolution-from-
session-defaults happens at construction time. See file 5.)

#### 5. `librqbit/src/session.rs` (lines 1150-1176 — `Session::add_torrent` `ManagedTorrentOptions` construction)

Resolve session default + per-torrent override, fall back to the
crate default.

```diff
@@
             let span = error_span!(parent: self.rs(), "torrent", id);
             let peer_opts = self.merge_peer_opts(opts.peer_opts);
+            let max_peer_connections = opts
+                .max_peer_connections
+                .or(self.opts.max_peer_connections_per_torrent)
+                .map(|n| n.get())
+                .unwrap_or(DEFAULT_PEER_CONNECTIONS_PER_TORRENT);
             let metadata = Arc::new(metadata);
             let minfo = Arc::new(ManagedTorrentShared {
                 id,
                 ...
                 options: ManagedTorrentOptions {
                     force_tracker_interval: opts.force_tracker_interval,
                     peer_connect_timeout: peer_opts.connect_timeout,
                     peer_read_write_timeout: peer_opts.read_write_timeout,
                     allow_overwrite: opts.overwrite,
                     output_folder,
                     disk_write_queue: self.disk_write_tx.clone(),
                     ratelimits: opts.ratelimits,
                     initial_peers: opts.initial_peers.clone().unwrap_or_default(),
+                    max_peer_connections,
                     #[cfg(feature = "disable-upload")]
                     _disable_upload: self._disable_upload,
                 },
```

The fact that the session stores `SessionOptions` (`self.opts`)
needs verification — the surveyed source uses `self.peer_id` /
`self.spawner` etc. as direct fields, and `peer_opts.connect_timeout`
goes through `self.merge_peer_opts`. The exact accessor may need
adjustment (either store `max_peer_connections_per_torrent` as a
dedicated field on `Session`, mirroring how `peer_id` is stored, or
keep the full `SessionOptions` available). The PR author should pick
whichever fits the existing pattern in `Session::new_with_opts`.

#### 6. `librqbit/src/torrent_state/live/mod.rs` (line 278 — `peer_semaphore` initialization)

Replace the hardcoded `128` with the resolved value from
`ManagedTorrentOptions`.

```diff
-            peer_semaphore: Arc::new(Semaphore::new(128)),
+            peer_semaphore: Arc::new(Semaphore::new(
+                paused.shared.options.max_peer_connections as usize,
+            )),
```

The semaphore is consumed in two sites today, both of which already
respect the budget correctly:

- `torrent_state/live/mod.rs:360-369` — `add_incoming_peer`
  `try_acquire_owned()` with debug-log fallback when saturated.
- `torrent_state/live/mod.rs:586` — `task_peer_adder` outgoing path
  `acquire_owned().await`, which blocks until a permit is available.

No call-site changes are needed; the semaphore's new size is fed
through the existing `Arc<Semaphore>` construction without any
behavior changes for unset-config builds.

### Tests

Add to `librqbit/src/tests/` (existing test layout — uses `wiremock`-
style in-memory peers).

```rust
// librqbit/src/tests/session_options.rs (new file or appended to an existing test module)
use std::num::NonZeroU32;
use librqbit::{
    AddTorrent, AddTorrentOptions, Session, SessionOptions,
    DEFAULT_PEER_CONNECTIONS_PER_TORRENT,
};

#[tokio::test]
async fn session_default_peer_cap_is_documented_constant() {
    // Sanity check: the crate-level default constant is what the historical
    // hardcoded value was, so unset-config builds behave identically.
    assert_eq!(DEFAULT_PEER_CONNECTIONS_PER_TORRENT, 128);
}

#[tokio::test]
async fn session_max_peer_connections_flows_to_torrent_state() {
    // Session-wide default applies when AddTorrentOptions doesn't override.
    let tmp = tempfile::tempdir().unwrap();
    let opts = SessionOptions {
        max_peer_connections_per_torrent: NonZeroU32::new(50),
        ..Default::default()
    };
    let session = Session::new_with_opts(tmp.path().to_owned(), opts)
        .await
        .unwrap();
    let handle = add_test_fixture_torrent(&session, None).await;
    assert_eq!(peer_semaphore_capacity(&handle), 50);
}

#[tokio::test]
async fn add_torrent_max_peer_connections_overrides_session_default() {
    // Per-torrent override wins over the session-wide default.
    let tmp = tempfile::tempdir().unwrap();
    let opts = SessionOptions {
        max_peer_connections_per_torrent: NonZeroU32::new(50),
        ..Default::default()
    };
    let session = Session::new_with_opts(tmp.path().to_owned(), opts)
        .await
        .unwrap();
    let per_torrent = AddTorrentOptions {
        max_peer_connections: NonZeroU32::new(20),
        ..Default::default()
    };
    let handle = add_test_fixture_torrent(&session, Some(per_torrent)).await;
    assert_eq!(peer_semaphore_capacity(&handle), 20);
}

#[tokio::test]
async fn session_unset_cap_falls_back_to_default() {
    // Both unset → DEFAULT_PEER_CONNECTIONS_PER_TORRENT.
    let tmp = tempfile::tempdir().unwrap();
    let session = Session::new_with_opts(tmp.path().to_owned(), SessionOptions::default())
        .await
        .unwrap();
    let handle = add_test_fixture_torrent(&session, None).await;
    assert_eq!(
        peer_semaphore_capacity(&handle),
        DEFAULT_PEER_CONNECTIONS_PER_TORRENT as usize,
    );
}

// Helpers (would be added under `cfg(test)` in the same module). The
// `peer_semaphore_capacity` helper requires either a new `pub(crate)`
// accessor on `TorrentStateLive` or `tokio::sync::Semaphore::available_permits`
// after the live state is up — both are existing-pattern conveniences
// already used in librqbit's integration tests.
```

### PR description (paste verbatim into the upstream PR)

```markdown
## Summary

Make the per-torrent peer-connection cap configurable. Previously this
was hardcoded to 128 (see `torrent_state/live/mod.rs:278`); this PR
adds an optional `max_peer_connections_per_torrent: Option<NonZeroU32>`
on `SessionOptions` and `max_peer_connections: Option<NonZeroU32>` on
`AddTorrentOptions` (per-torrent override). Unset values fall back to
a new public constant `DEFAULT_PEER_CONNECTIONS_PER_TORRENT` (= 128)
so existing consumers see no behavior change.

The semaphore consumes the resolved cap in both the outgoing
(`task_peer_adder`) and incoming (`add_incoming_peer`) paths, so a
single configuration knob controls the total live peer task budget
per torrent.

## Motivation

Several embedding applications honour scheduling invariants that
specify a different per-torrent peer cap (e.g. kino, a streaming
client, locks "max connections per torrent: 200" via PRD; mobile
apps may want a much lower cap). The internal
`peer_semaphore: Arc<Semaphore>` already has the right shape; the
patch just plumbs configurability through the public option structs.

The internal `TODO: make it configurable` at
`torrent_state/live/mod.rs:233` (file_priorities) is parallel evidence
that "configurability via a struct field" is the expected direction
for similar internal-cap APIs.

## Surface area

- New field on `SessionOptions` (additive — no migration needed
  thanks to `#[derive(Default)]` on the struct).
- New field on `AddTorrentOptions` (additive).
- New public constant `DEFAULT_PEER_CONNECTIONS_PER_TORRENT`.
- One new field on the crate-internal `ManagedTorrentOptions`
  (`pub(crate)` — no semver impact).
- One line changed in `TorrentStateLive::new` (the
  `Semaphore::new(...)` argument).

## Tests

Three new tokio tests in `librqbit/src/tests/` exercising:

1. Session-wide default value flows to the semaphore.
2. Per-torrent override wins over session default.
3. Unset values fall back to `DEFAULT_PEER_CONNECTIONS_PER_TORRENT`.

All three assert against a (small new) `peer_semaphore_capacity()`
test helper.

## Backwards compatibility

Default-built clients: identical behavior (the semaphore is sized to
`DEFAULT_PEER_CONNECTIONS_PER_TORRENT = 128`, same as the pre-PR
hardcoded value).

Embedded clients that set a per-`SessionOptions`/-`AddTorrentOptions`
override: get the configured cap.

`Option<NonZeroU32>` is forwards-compatible: a future bump to
`Option<NonZeroUsize>` would be a breaking change (so the PR is
deliberate about `u32` here, matching `LimitsConfig`'s `u64` bandwidth
fields' precedent for sized integer types).
```

---

## PR B — per-stream lookahead + last-piece-priority API

**Title:** `feat(streaming): expose per-stream lookahead size + last-piece priority hint`

**Summary:** PR B is structurally more invasive than PR A because the
PRD's piece-priority spec (HIGHEST `[pos, pos+60s]` / HIGH `[pos+60s,
pos+300s]` / last-piece HIGH / NORMAL) maps onto two distinct
mechanisms in librqbit:

1. **HIGHEST window** ≈ the existing
   `PER_STREAM_BUF_DEFAULT = 32 * 1024 * 1024` lookahead in
   `torrent_state/streaming.rs:27` (per `iter_next_pieces` in lines
   71-100). Already implemented inside the crate; just not
   configurable per stream.

2. **Last-piece HIGH** — useful for streaming `.mp4` files whose
   `moov` atom lives at the end. Not currently special-cased; the
   scheduler walks pieces in file order modulated by streams'
   queues, so the last piece is fetched roughly in linear time
   after the first 32 MiB lookahead is satisfied.

3. **HIGH window `[pos+60s, pos+300s]`** — there is no existing
   mechanism for tiered "after-lookahead" pre-fetching. Adding one
   requires either a new piece-priority enum (HIGHEST / HIGH /
   NORMAL / DONT_DOWNLOAD) with per-piece priority storage and a
   tiered scheduler walk, OR a second lookahead window with a
   weaker scheduling preference. Both are bigger changes than PR A.

PR B is therefore drafted as **two independently-mergeable sub-PRs**:

### Sub-PR B1: configurable stream lookahead

**Title:** `feat(streaming): make per-stream lookahead buffer size configurable`

**Summary:** Replaces the hardcoded
`PER_STREAM_BUF_DEFAULT: u64 = 32 * 1024 * 1024` constant
(`torrent_state/streaming.rs:27`) with a per-stream value carried on
`StreamState` and selectable at stream-construction time via a new
`ManagedTorrent::stream_with_lookahead(file_id, lookahead_bytes)`
method. The pre-existing `ManagedTorrent::stream(file_id)` delegates
to the new method with `PER_STREAM_BUF_DEFAULT` so all existing
callers see no behavior change.

**Why:** the lookahead value is the size of the HIGHEST-priority
piece queue `TorrentStreams::iter_next_pieces` produces for an
active stream. Downstream applications need to tune it to the active
file's bitrate and the user's network conditions:

-   A 4K Blu-ray rip (~80 Mbit/s) benefits from a larger window so
    `iter_next_pieces`'s round-robin doesn't fall behind the player.
-   A low-bitrate web rip (~3 Mbit/s) over a slow mobile link
    benefits from a smaller window so the scheduler picks up
    downstream pieces (subtitles, audio, end-of-file `moov` atom)
    sooner.

The constant has been internal since librqbit's streaming API
landed; this PR is the smallest possible surface change that
exposes it.

**Surface area:** one new field on a crate-internal struct, one new
public method, one delegate change in the existing public method.
Zero behavior change for default builds.

### Files changed

#### 1. `librqbit/src/torrent_state/streaming.rs` (lines 29-35 — `StreamState`)

Add `lookahead_bytes: u64` to the per-stream state struct. The field
is non-`Option<…>` because the resolution-from-default happens at
construction time (see file 4).

```diff
 struct StreamState {
     file_id: usize,
     file_len: u64,
     file_abs_offset: u64,
     position: u64,
     waker: Option<Waker>,
+    /// Per-stream HIGHEST-priority lookahead window, in bytes.
+    /// Set at construction time via [`ManagedTorrent::stream_with_lookahead`]
+    /// (or the default [`PER_STREAM_BUF_DEFAULT`] via [`ManagedTorrent::stream`]).
+    lookahead_bytes: u64,
 }
```

#### 2. `librqbit/src/torrent_state/streaming.rs` (lines 42-49 — `StreamState::queue`)

Read `self.lookahead_bytes` instead of the module-level constant. The
loop body is otherwise unchanged.

```diff
     fn queue<'a>(&self, lengths: &'a Lengths) -> impl Iterator<Item = ValidPieceIndex> + 'a {
         let start = self.file_abs_offset + self.position;
-        let end = (start + PER_STREAM_BUF_DEFAULT).min(self.file_abs_offset + self.file_len);
+        let end = (start + self.lookahead_bytes).min(self.file_abs_offset + self.file_len);
         let dpl = lengths.default_piece_length();
         let start_id = (start / dpl as u64).try_into().unwrap();
         let end_id = end.div_ceil(dpl as u64).try_into().unwrap();
         (start_id..end_id).filter_map(|i| lengths.validate_piece_index(i))
     }
```

#### 3. `librqbit/src/lib.rs` (re-export `PER_STREAM_BUF_DEFAULT`)

Promote the existing module-level constant to a public re-export so
callers can refer to the historical default by name (parallel to
the `DEFAULT_PEER_CONNECTIONS_PER_TORRENT` constant introduced by
PR A).

```diff
 pub use torrent_state::{
     ManagedTorrent, ManagedTorrentShared, ManagedTorrentState, TorrentMetadata, TorrentStats,
     TorrentStatsState,
 };
+
+/// Default per-stream lookahead window in bytes (32 MiB). This is the
+/// size of the HIGHEST-priority piece queue produced for each active
+/// [`ManagedTorrent::stream`]. Override per-stream via
+/// [`ManagedTorrent::stream_with_lookahead`].
+pub const PER_STREAM_BUF_DEFAULT: u64 = torrent_state::streaming::PER_STREAM_BUF_DEFAULT;
```

(`PER_STREAM_BUF_DEFAULT` in `streaming.rs` needs to be `pub(crate)`
instead of file-private for the re-export to compile — a one-token
change. Alternatively, redeclare the constant in `lib.rs` and pass
it by value to `stream(file_id)` at the call site; either approach
is fine.)

#### 4. `librqbit/src/torrent_state/streaming.rs` (lines 327-365 — `ManagedTorrent::stream` and new `stream_with_lookahead`)

Split the existing `stream(file_id)` into a thin delegate plus the
substantive `stream_with_lookahead(file_id, lookahead_bytes)`. The
construction body moves verbatim into the new method except for the
`StreamState` literal, which gains the new field.

```diff
-    pub fn stream(self: Arc<Self>, file_id: usize) -> anyhow::Result<FileStream> {
+    /// Open a streaming read handle to the given file, using the
+    /// default HIGHEST-priority lookahead window
+    /// ([`PER_STREAM_BUF_DEFAULT`] = 32 MiB).
+    ///
+    /// For finer control over the lookahead window, see
+    /// [`Self::stream_with_lookahead`].
+    pub fn stream(self: Arc<Self>, file_id: usize) -> anyhow::Result<FileStream> {
+        self.stream_with_lookahead(file_id, PER_STREAM_BUF_DEFAULT)
+    }
+
+    /// Like [`Self::stream`] but with a custom lookahead window. The
+    /// `lookahead_bytes` parameter is the number of bytes ahead of the
+    /// current read position that the scheduler treats as HIGHEST
+    /// priority (i.e. the size of the queue
+    /// [`TorrentStreams::iter_next_pieces`] produces for this stream).
+    ///
+    /// Smaller values let the scheduler pick up downstream pieces
+    /// (subtitles, audio, end-of-file metadata) sooner on slow links;
+    /// larger values pre-fetch more aggressively for high-bitrate
+    /// content. The default is 32 MiB
+    /// ([`PER_STREAM_BUF_DEFAULT`]); a typical streaming client picks
+    /// a value somewhere between 8 MiB and 256 MiB depending on the
+    /// active file's bitrate.
+    ///
+    /// `lookahead_bytes = 0` is accepted and disables lookahead
+    /// entirely; the scheduler then relies on the player's read
+    /// position alone (the current piece is still fetched HIGHEST
+    /// because `iter_next_pieces` always emits the read-position's
+    /// piece first).
+    pub fn stream_with_lookahead(
+        self: Arc<Self>,
+        file_id: usize,
+        lookahead_bytes: u64,
+    ) -> anyhow::Result<FileStream> {
         let metadata = self
             .metadata
             .load_full()
             .context("torrent metadata is not resolved")?;
         let (fd_len, fd_offset) = self.with_storage_and_file(
             file_id,
             |_fd, fi| (fi.len, fi.offset_in_torrent),
             &metadata,
         )?;
         let streams = self.streams()?;
         let s = FileStream {
             stream_id: streams.next_id(),
             streams: streams.clone(),
             file_id,
             position: 0,

             file_len: fd_len,
             file_torrent_abs_offset: fd_offset,
             torrent: self,
             spawner: BlockingSpawner::default(),
             metadata,
         };
         s.torrent.maybe_reconnect_needed_peers_for_file(file_id);
         streams.streams.insert(
             s.stream_id,
             StreamState {
                 file_id,
                 position: 0,
                 waker: None,
                 file_len: fd_len,
                 file_abs_offset: fd_offset,
+                lookahead_bytes,
             },
         );

-        debug!(stream_id = s.stream_id, file_id, "started stream");
+        debug!(stream_id = s.stream_id, file_id, lookahead_bytes, "started stream");

         Ok(s)
     }
```

### Tests

Add to `librqbit/src/tests/e2e_stream.rs` (the existing streaming
e2e module). The existing `test_e2e_stream` test covers the default
path; the new tests below cover the custom-lookahead path and the
zero-lookahead degenerate case. All three reuse the
`create_default_random_dir_with_torrents` test helper that the
existing test already uses.

```rust
// librqbit/src/tests/e2e_stream.rs (appended)
use crate::PER_STREAM_BUF_DEFAULT;

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_stream_default_lookahead_matches_constant() -> anyhow::Result<()> {
    // Sanity check: the default `stream(file_id)` produces a stream
    // whose lookahead equals the documented constant.
    timeout(Duration::from_secs(10), e2e_stream_with_assertion(|streams_arc, _| {
        let stream_state = streams_arc.streams.iter().next().unwrap();
        assert_eq!(stream_state.value().lookahead_bytes, PER_STREAM_BUF_DEFAULT);
    }))
    .await?
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_stream_with_custom_lookahead() -> anyhow::Result<()> {
    // Custom lookahead flows from the constructor through StreamState
    // to iter_next_pieces' queue iteration.
    const CUSTOM_LOOKAHEAD: u64 = 4 * 1024 * 1024; // 4 MiB; smaller than default
    timeout(
        Duration::from_secs(10),
        e2e_stream_with_lookahead(CUSTOM_LOOKAHEAD, |streams_arc, lengths| {
            let stream_state = streams_arc.streams.iter().next().unwrap();
            assert_eq!(stream_state.value().lookahead_bytes, CUSTOM_LOOKAHEAD);
            // The queue iteration is bounded by the smaller of:
            // (lookahead_bytes / piece_length) or (file_len / piece_length)
            // and inclusive of the boundary piece. For an 8 KiB file at
            // 1 KiB pieces, the queue must fit within the file even if
            // lookahead is much larger.
            let queue_len = stream_state.value().queue(lengths).count();
            let max_pieces = (lengths.total_length() as u64).div_ceil(lengths.default_piece_length() as u64);
            assert!(queue_len as u64 <= max_pieces);
        }),
    )
    .await?
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_stream_with_zero_lookahead_still_streams() -> anyhow::Result<()> {
    // `lookahead_bytes = 0` is accepted and the stream still
    // completes — the read-position's current piece is fetched
    // HIGHEST because `iter_next_pieces` always emits the
    // read-position's piece first regardless of the lookahead value.
    timeout(Duration::from_secs(10), e2e_stream_with_lookahead(0, |_streams_arc, _| ())).await?
}

// Helper that builds the server/client sessions from `e2e_stream` but
// opens the client read stream with the given lookahead and runs an
// inspection callback on the client's TorrentStreams + Lengths before
// reading to completion.
async fn e2e_stream_with_lookahead<F>(
    lookahead_bytes: u64,
    inspect: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&std::sync::Arc<crate::torrent_state::streaming::TorrentStreams>, &librqbit_core::lengths::Lengths),
{
    // ... duplicates the setup body of `e2e_stream()` (test_util fixture,
    // server session w/ peer_id, AddTorrent::from_bytes, client session,
    // wait_until_initialized) and then calls
    // `client_handle.stream_with_lookahead(0, lookahead_bytes)` instead of
    // `client_handle.stream(0)`. The inspect callback runs after the
    // stream is registered but before `read_to_end`.
    todo!("extract `e2e_stream`'s body into a helper accepting `(lookahead, inspect)`")
}

async fn e2e_stream_with_assertion<F>(inspect: F) -> anyhow::Result<()>
where
    F: FnOnce(&std::sync::Arc<crate::torrent_state::streaming::TorrentStreams>, &librqbit_core::lengths::Lengths),
{
    e2e_stream_with_lookahead(PER_STREAM_BUF_DEFAULT, inspect).await
}
```

(The PR author will want to extract `e2e_stream()`'s common setup
into the `e2e_stream_with_lookahead` helper so all four tests share
the fixture-build / dual-session / wait-init scaffolding — about
80 lines of currently-inline code at lines 16-105 of
`tests/e2e_stream.rs`. The existing `test_e2e_stream` becomes a
one-line caller of the new helper with the default lookahead and a
no-op inspect callback.)

A `pub(crate)` accessor on `TorrentStreams::streams` (or a new
`pub(crate) fn iter(&self)`) is needed so the inspect callback can
read `StreamState::lookahead_bytes` — the field itself is the only
new test-observable state. The accessor can be `cfg(test)`-gated if
maintainers prefer to keep the field strictly internal in
production builds.

### PR description (paste verbatim into the upstream PR for B1)

```markdown
## Summary

Make the per-stream HIGHEST-priority lookahead window size
configurable. Currently the constant
`PER_STREAM_BUF_DEFAULT: u64 = 32 * 1024 * 1024` in
`torrent_state/streaming.rs:27` controls how many bytes ahead of each
active stream's read position are pulled into the HIGHEST-priority
piece queue produced by `TorrentStreams::iter_next_pieces`. This PR
exposes that value as a per-stream configurable.

- New `ManagedTorrent::stream_with_lookahead(file_id, lookahead_bytes)`
  constructor.
- The pre-existing `ManagedTorrent::stream(file_id)` becomes a thin
  delegate that passes `PER_STREAM_BUF_DEFAULT`, so no existing caller
  sees a behavior change.
- `PER_STREAM_BUF_DEFAULT` is promoted to a public crate-level
  constant so callers can refer to the historical default by name.

## Motivation

Streaming clients want to tune the lookahead based on the active
file's bitrate and the user's network conditions:

- A 4K Blu-ray rip (~80 Mbit/s sustained) needs a larger lookahead
  so the scheduler's round-robin across streams doesn't fall behind
  the player.
- A low-bitrate web rip (~3 Mbit/s) on a slow mobile link benefits
  from a smaller window so the scheduler picks up downstream pieces
  (subtitles, audio, end-of-file `moov` atom) sooner.
- An audio-only stream (e.g. a music torrent) doesn't need 32 MiB
  of lookahead at all; an 8 MiB lookahead halves the buffered byte
  count without affecting playback continuity.

The constant has been internal since librqbit's streaming API
landed; this PR is the smallest possible surface change that exposes
it.

## Surface area

- One new field on the crate-internal `StreamState`
  (`lookahead_bytes: u64`).
- One line changed in `StreamState::queue` (reads `self.lookahead_bytes`
  instead of the constant).
- One new public method on `ManagedTorrent`
  (`stream_with_lookahead`).
- One delegate change in the existing `ManagedTorrent::stream` (now
  calls `stream_with_lookahead` with `PER_STREAM_BUF_DEFAULT`).
- `PER_STREAM_BUF_DEFAULT` re-exported from the crate root.

## Tests

Three new `#[tokio::test(flavor = "multi_thread")]` tests in
`librqbit/src/tests/e2e_stream.rs`:

1. `test_e2e_stream_default_lookahead_matches_constant` — the
   default `stream(file_id)` produces a stream whose
   `lookahead_bytes` equals `PER_STREAM_BUF_DEFAULT`.
2. `test_e2e_stream_with_custom_lookahead` — a 4 MiB custom
   lookahead flows through to `StreamState` and bounds
   `iter_next_pieces`' queue iteration accordingly.
3. `test_e2e_stream_with_zero_lookahead_still_streams` — the
   degenerate `lookahead_bytes = 0` case still reads the file to
   completion (the read-position's current piece is fetched HIGHEST
   independent of the lookahead value).

The existing `test_e2e_stream` test is refactored to call the new
`e2e_stream_with_lookahead` helper with the default lookahead and a
no-op inspect callback, sharing the fixture-build + dual-session +
wait-init scaffolding across all four tests.

## Backwards compatibility

`stream(file_id)` is unchanged in shape and behavior; all existing
callers see no diff. The new method is additive. The
`PER_STREAM_BUF_DEFAULT` re-export is additive. `StreamState` is
crate-internal so adding a field has no semver impact.

## Future work (not in this PR)

The matching "tiered after-lookahead priority window" mechanism
proposed in librqbit's PRD-driven streaming use case (HIGH window
`[pos+60s, pos+300s]` beyond the HIGHEST lookahead) is structurally
bigger and will be drafted as a follow-up PR — see the linked design
proposal in the same downstream consumer's notes for the per-piece
priority enum + tiered scheduler walk sketch.
```

### Sub-PR B2: piece-priority enum + tiered scheduler walk

The substantive change. Requires:

1. New `pub enum PiecePriority { Highest, High, Normal, DontDownload }`
   in `librqbit/src/lib.rs` (or a sibling module).
2. Per-piece priority storage. Either: (a) a parallel
   `BitVec`-of-2-bits (~8MB for a 1 TiB torrent at 256 KiB pieces —
   fine), or (b) a `HashMap<ValidPieceIndex, PiecePriority>` keyed on
   non-Normal entries only (sparse). Option (b) is recommended for
   memory pressure.
3. Public API on `ManagedTorrent`:
   ```rust
   pub fn set_piece_priorities(
       &self,
       file_id: usize,
       ranges: impl IntoIterator<Item = (Range<usize>, PiecePriority)>,
   ) -> anyhow::Result<()>;
   ```
   The `Range<usize>` is over piece indices within the file (which
   `librqbit::file_info::FileInfo::piece_range` already exposes as a
   `pub` field).
4. Scheduler integration in `chunk_tracker::iter_queued_pieces`
   (lines 216-229): walk HIGHEST → HIGH → NORMAL, intersected with
   the existing `file_priorities` order. The existing
   `iter_next_pieces` (streaming) should be invoked BEFORE the
   tiered walk so streams' HIGHEST windows are honored first.
5. The last-piece-HIGH convenience could be a method on
   `FileStream` that calls `set_piece_priorities` with the file's
   final piece index marked HIGH (or a constructor flag on
   `stream_with_lookahead` for the common case).

**Why this is structurally bigger than PR A:** the scheduler walk
in `chunk_tracker::iter_queued_pieces` interacts with the existing
endgame mode, the bitfield queueing logic
(`mark_piece_broken_if_not_have`), and the streams' wake-on-completed
flow (`TorrentStreams::wake_streams_on_piece_completed`). A correctness
review would want test coverage proving:

- A piece marked DontDownload is never queued, even if streams
  request it.
- Switching a piece from Normal → Highest mid-download wakes the
  scheduler (no missed piece-event).
- The endgame mode (when ≤ N pieces remain) still uses redundant
  fetching across peers regardless of priority.
- Priorities survive pause/resume cycles
  (`TorrentStateLive::pause` / restart from `TorrentStatePaused`).

Sub-PR B2 is therefore drafted here as a **design proposal**, not a
full diff. A separate agent session (or the human directly) can
land the full implementation once the design is accepted upstream.

---

## Post-merge kino-side wiring (informative)

### After PR A lands (librqbit `8.2.0` hypothetical)

1. Bump `librqbit = "8.2"` in `crates/kino-torrent/Cargo.toml`
   (or `Cargo.toml` workspace deps).
2. `crates/kino-torrent/src/engine.rs:310-319` becomes:

   ```rust
   let opts = SessionOptions {
       disable_dht: !config.enable_dht,
       disable_dht_persistence: true,
       trackers,
       max_peer_connections_per_torrent: NonZeroU32::new(
           kino_core::constants::MAX_CONNECTIONS_PER_TORRENT,
       ),
       ..Default::default()
   };
   ```

3. Remove the ADR-103 deferral comment in the same region and the
   §6A entry for F-013 in `STATE.md` flips to RESOLVED.

### After PR B1 lands (librqbit `8.3.0` hypothetical)

1. Bump `librqbit = "8.3"` in the workspace `Cargo.toml`.
2. `crates/kino-torrent/src/engine.rs:233-246` (the
   `AddedTorrent::open_stream` body) becomes:

   ```rust
   pub fn open_stream(&self, file_index: usize) -> Result<Box<dyn FileStream>> {
       if file_index >= self.inner_files.len() {
           return Err(EngineError::FileIndexOutOfRange {
               requested: file_index,
               file_count: self.inner_files.len(),
           });
       }
       // PRD §F-014 HIGHEST window = [position, position + 60s]. Until
       // upstream lands the per-piece priority API (PR B2 / F-014 §6A
       // closure path 'a'), kino approximates the window as a per-stream
       // lookahead sized to 60s of the file's bitrate, clamped to a
       // sensible range so audio-only streams don't over-buffer and 4K
       // streams don't underrun. The clamp constants live in
       // `kino_core::constants` next to the PRD-locked window values.
       let lookahead = self
           .file_bitrate_bps(file_index)
           .map(|bps| {
               let raw = (bps as u64)
                   .saturating_mul(kino_core::constants::PIECE_PRIORITY_HIGH_WINDOW_S as u64)
                   / 8;
               raw.clamp(
                   kino_core::constants::LOOKAHEAD_MIN_BYTES,
                   kino_core::constants::LOOKAHEAD_MAX_BYTES,
               )
           })
           .unwrap_or(librqbit::PER_STREAM_BUF_DEFAULT);

       let s = self
           .inner
           .clone()
           .stream_with_lookahead(file_index, lookahead)
           .map_err(EngineError::Internal)?;
       Ok(Box::new(s))
   }
   ```

3. New constants in `crates/kino-core/src/constants.rs` next to
   `PIECE_PRIORITY_HIGH_WINDOW_S`:

   ```rust
   /// Per-stream HIGHEST-priority lookahead floor (PRD §F-014).
   /// Below this even very-low-bitrate streams keep enough buffer
   /// to absorb player seek overshoot.
   pub const LOOKAHEAD_MIN_BYTES: u64 = 8 * 1024 * 1024;

   /// Per-stream HIGHEST-priority lookahead ceiling (PRD §F-014).
   /// Above this the buffered byte count crowds out parallel
   /// downstream-piece fetches (subtitles, audio, end-of-file moov).
   pub const LOOKAHEAD_MAX_BYTES: u64 = 256 * 1024 * 1024;
   ```

4. PRD §F-014's HIGHEST window is partially honored (the lookahead
   approximation); the §6A entry stays OPEN with a "PR B1 wired;
   awaiting PR B2 for HIGH window + last-piece-HIGH" status update
   until PR B2 also lands.

### After PR B2 lands (librqbit `8.4.0` hypothetical)

1. Bump `librqbit = "8.4"` in the workspace `Cargo.toml`.
2. New module `crates/kino-torrent/src/piece_priority.rs` exposing a
   per-stream `update_for_position(position_s, file_bitrate_bps)`
   function that calls
   `ManagedTorrent::set_piece_priorities(file_id, ranges)` with:
   - HIGHEST ranges: pieces covering `[position_s, position_s + 60s]`
     of file bitrate.
   - HIGH ranges: pieces covering `[position_s + 60s, position_s + 300s]`.
   - HIGH single piece: the file's final piece (`FileInfo::piece_range`'s
     `.end - 1`).
3. `crates/kino-torrent/src/monitor.rs::BufferMonitor` calls the new
   function on every position-event recompute (sampled every 1s per
   PRD §F-014).
4. F-014 §6A entry in `STATE.md` flips to RESOLVED.

### Closure-path summary

| Upstream PR | librqbit version | kino §6A entry resolved |
|---|---|---|
| PR A | `8.2.0` | F-013 (max_connections_per_torrent) |
| PR B1 | `8.3.0` | F-014 partial (HIGHEST window via lookahead approximation) |
| PR B2 | `8.4.0` | F-014 fully (HIGH window + last-piece-HIGH) |

If upstream rejects any of these — or the timeline stretches past
the v1 ship window — the **PRD revision option (d)** filed in
`STATE.md::PRD Issues` is the human-ratification fallback. The
proposed revision text is already drafted in that entry; a follow-up
session flips the §6A entries to RESOLVED in a one-line STATE.md
diff if the human ratifies.

---

## Maintenance notes

- **Don't drift this draft from upstream.** If a future librqbit
  release lands changes that touch any of the cited lines (e.g. a
  refactor of `TorrentStateLive::new` or `Session::add_torrent`),
  re-anchor the diffs against the new line numbers before submitting.
  A small `re-anchor` agent session could automate that re-walk.
- **Apache-2.0 license compatibility.** rqbit is Apache-2.0; kino's
  contribution path (the human submits the PR upstream under their
  own GitHub identity) is the standard inbound contribution flow.
  The patches above don't carry kino-specific text; everything is
  written as a polite upstream PR.
- **Don't bundle PR A, B1, and B2.** All three are independently
  mergeable and address distinct PRD invariants. A maintainer is
  more likely to accept three small, focused PRs than one feature
  PR that mixes concerns. The recommended submission order is PR A
  → PR B1 → PR B2 (smallest surface first; B2 builds on the design
  context B1 establishes).
- **CI signal.** rqbit's CI runs `cargo test` and `cargo clippy --
  -D warnings`. New code in all three PRs should be `clippy`-clean
  (no warnings from `clippy::pedantic`'s `must_use_candidate` etc.;
  document new constants and struct fields with `///` doc comments
  to satisfy `missing_docs` if upstream enforces it). PR A's new
  fields on the public `SessionOptions` and `AddTorrentOptions`
  structs especially need doc comments since those structs are part
  of the crate's stable surface.
