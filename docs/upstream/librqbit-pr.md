# Upstream librqbit PR drafts (F-013 / F-014 §6A closure path)

**Status:** draft, ready for human review before upstream submission.
**Target upstream:** `ikatson/rqbit` (Apache-2.0).
**librqbit version surveyed:** `8.1.1` (Cargo.lock pin; latest on crates.io
as of 2026-05-19).
**Filed by:** kino agent Session 039 (PR A + PR B framing); Sub-PR B1
expanded to ready-to-submit in Session 040; Sub-PR B2 expanded to
ready-to-submit in Session 041.

This document drafts three upstream changes that, if accepted by the rqbit
maintainer, would close the two §6A code-acceptance regressions kino's
PRD Issues entry tracks for F-013 ("max connections per torrent: 200")
and F-014 ("piece priorities mapped to librqbit ..."). The drafts are
written so that the human can copy each PR description verbatim into a
GitHub PR; the diffs are anchored to real lines in librqbit 8.1.1's
on-disk source tree as cross-checked from
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/librqbit-8.1.1/`
(or, in fresh containers where the registry cache is empty, against the
tarball at
`https://static.crates.io/crates/librqbit/librqbit-8.1.1.crate`).

Until upstream lands, kino's §6A entries stay OPEN. Once upstream
publishes a release containing **PR A** (the smaller change), the
follow-up kino session bumps `librqbit` in `Cargo.toml`, wires the
new field through `kino_torrent::engine`, and flips the F-013 §6A
entry to RESOLVED. **PR B** is split into two independently-mergeable
sub-PRs: **PR B1** (per-stream lookahead, drafted fully) and **PR B2**
(per-piece priority enum + tiered scheduler walk, also drafted fully
as of Session 041). Wiring B1 honors PRD §F-014's HIGHEST window
approximately (lookahead sized to 60s of file bitrate); B2 adds the
explicit HIGH window and the last-piece-HIGH special case. If upstream
rejects or stalls any of A/B1/B2, the PRD Issues entry's **option (d)**
(PRD revision relaxing the language to "best-effort, subject to engine
API capabilities") remains the §6A clearance lever.

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
   requires a new piece-priority enum (HIGHEST / HIGH / NORMAL /
   DONT_DOWNLOAD) with per-piece priority storage and a tiered
   scheduler walk. This is structurally larger than B1 because it
   touches both the schedule-walk in `chunk_tracker` and the
   in-flight reservation in `torrent_state/live`, and it adds two new
   public surface elements (the enum + the setter method on
   `ManagedTorrent`).

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

**Title:** `feat(streaming): per-piece priority API for tiered scheduler walk`

**Summary:** Adds a public `PiecePriority` enum
(`Highest` / `High` / `Normal` / `DontDownload`) and a setter method
`ManagedTorrent::set_piece_priorities(file_id, ranges)` so external
callers can override the scheduler's piece selection on a per-piece
basis. The reservation loop in `TorrentStateLive::reserve_next_needed_piece`
gains two new walk tiers (HIGHEST then HIGH) inserted between the
existing per-stream lookahead (`TorrentStreams::iter_next_pieces`) and
the natural file-priority walk (`ChunkTracker::iter_queued_pieces`).
`DontDownload`-marked pieces are filtered out of all three walks so
they're never reserved, even if an active stream's lookahead happens to
cover them. The new state lives in a sibling `Arc<TorrentPiecePriorities>`
field on `TorrentStateLive` and `TorrentStatePaused`, parallel to the
existing `Arc<TorrentStreams>` — created once per torrent in
`TorrentStateInitializing::check()` and preserved across pause/resume
cycles via the same Arc-clone mechanism `streams` already uses.

**Why:** PRD §F-014 (the kino streaming client's locked design)
specifies a four-tier piece-priority model that maps onto librqbit's
existing scheduler thus:

| PRD tier | librqbit mechanism |
|---|---|
| HIGHEST (`[pos, pos+60s]`) | Existing stream lookahead (`PER_STREAM_BUF_DEFAULT`, configurable after PR B1) |
| **HIGH (`[pos+60s, pos+300s]`)** | **No existing mechanism — PR B2 adds it** |
| **HIGH (last piece of active file)** | **No existing mechanism — PR B2 adds it** |
| NORMAL (everything else) | Existing file-priority walk (`iter_queued_pieces`) |

The HIGHEST tier is already handled by the per-stream lookahead window
(B1 makes it configurable per file). The HIGH tier — pieces just
beyond the lookahead window, which the player will want next but
isn't actively reading — needs an explicit priority annotation
because the scheduler's "natural order" walks file-by-file then
piece-by-piece, with no notion of "second-tier priority".

The same enum's `DontDownload` variant is independently useful for
"skip this episode" / "don't download disc 2" UI affordances —
downstream callers can mark whole files as DontDownload without
having to use `update_only_files` (which atomically rewrites the
selected set and is heavier than a per-piece annotation).

`Last-piece-HIGH` (the `.mp4` `moov`-atom case) is left to the
caller: they call `set_piece_priorities(file_id, [(file_pieces - 1
..file_pieces, High)])` once at stream open. No new convenience
method is added in this PR; if usage shows the call site duplicating
the same boilerplate, a follow-up PR can add a thin wrapper.

**Surface area:**
- One new public enum (`PiecePriority`) in a new module
  `librqbit/src/torrent_state/piece_priorities.rs`, re-exported from
  `librqbit/src/lib.rs`.
- One new crate-internal struct (`TorrentPiecePriorities`) in the same
  module, with `Default` + four `pub(crate)` methods.
- One new field on each of `TorrentStateLive` and `TorrentStatePaused`
  (sibling to the existing `streams: Arc<TorrentStreams>` field).
- One new public method on `ManagedTorrent` (`set_piece_priorities`)
  and one read-back accessor (`piece_priorities`).
- One updated method body on `TorrentStateLive::reserve_next_needed_piece`
  (lines 1227-1276) — two new tier walks chained into the existing
  for-loop's `priority_streamed_pieces.chain(natural_order_pieces)`.
- Zero behavior change for callers that never call `set_piece_priorities`:
  the default `TorrentPiecePriorities` is empty, all tier walks yield
  nothing, the DontDownload filter degrades to a constant `true`.

### Files changed

#### 1. `librqbit/src/torrent_state/piece_priorities.rs` (NEW)

New module hosting the `PiecePriority` enum and the
`TorrentPiecePriorities` storage. The module is created here because
the storage shape (per-piece map, keyed by `ValidPieceIndex`, only
touched in the scheduler and the public setter) doesn't naturally fit
in any existing file: it's not stream state (lives in `streaming.rs`),
not chunk-level state (lives in `chunk_tracker.rs`), not session-wide
config (lives in `session.rs`). The module is ~120 LOC; placing it as
a sibling to `streaming.rs` keeps the `torrent_state/` directory's
"one concern per file" layout intact.

```rust
//! Per-piece priority storage for the scheduler tiered walk.
//!
//! This module backs [`ManagedTorrent::set_piece_priorities`] and is
//! consulted by [`crate::torrent_state::live::TorrentStateLive::reserve_next_needed_piece`]
//! between the per-stream lookahead walk and the natural file-priority
//! walk. The storage is sparse: only non-`Normal` entries are kept, so a
//! torrent that never calls `set_piece_priorities` pays zero memory cost.
//!
//! The struct is preserved across pause/resume cycles via the same
//! `Arc`-clone path as [`crate::torrent_state::streaming::TorrentStreams`]:
//! it is created once per torrent during initialization and lives on
//! both [`crate::torrent_state::live::TorrentStateLive`] and
//! [`crate::torrent_state::paused::TorrentStatePaused`].

use std::collections::HashMap;

use librqbit_core::lengths::ValidPieceIndex;
use parking_lot::RwLock;

/// Per-piece scheduling priority override.
///
/// Set via [`crate::ManagedTorrent::set_piece_priorities`]. Pieces
/// without an explicit annotation are treated as [`PiecePriority::Normal`]
/// (the default).
///
/// # Tier ordering
///
/// The scheduler walks tiers in this order on every peer's next-piece
/// request: per-stream lookahead → `Highest` → `High` → `Normal`. A
/// piece marked `DontDownload` is filtered out of all four walks and is
/// never reserved, even if an active stream's lookahead window happens
/// to cover it.
///
/// # When to use each tier
///
/// - `Highest`: when the caller knows the piece is needed immediately
///   AND is NOT covered by an active stream (use the stream's lookahead
///   for that — it has identical priority effect and is cheaper).
/// - `High`: when the piece is needed soon but not immediately
///   (e.g. PRD §F-014's `[pos+60s, pos+300s]` HIGH window for kino's
///   adaptive buffer, or the last piece of an `.mp4` file whose `moov`
///   atom lives at the end).
/// - `Normal`: the implicit default — no annotation needed, but the
///   variant exists so callers can explicitly UN-set a previous
///   `Highest`/`High`/`DontDownload` annotation.
/// - `DontDownload`: when the caller wants the scheduler to skip the
///   piece entirely. Useful for "skip episode" / "skip bonus disc" UI
///   without going through `update_only_files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PiecePriority {
    /// Walk before `High` and `Normal`. See type-level docs.
    Highest,
    /// Walk before `Normal`. See type-level docs.
    High,
    /// The implicit default. See type-level docs.
    #[default]
    Normal,
    /// Skip entirely. See type-level docs.
    DontDownload,
}

/// Sparse per-piece priority store used by the scheduler.
///
/// Pieces not present in the inner map are treated as
/// [`PiecePriority::Normal`]; the empty map (the default state) thus
/// represents "no priorities set" without any allocation.
#[derive(Default)]
pub(crate) struct TorrentPiecePriorities {
    /// Sparse map: only non-`Normal` entries stored. Pieces tagged
    /// `Normal` are removed from the map by [`Self::set_many`].
    inner: RwLock<HashMap<ValidPieceIndex, PiecePriority>>,
}

impl TorrentPiecePriorities {
    /// Read the current priority for a single piece. Returns
    /// [`PiecePriority::Normal`] if the piece has no explicit
    /// annotation.
    pub(crate) fn get(&self, idx: ValidPieceIndex) -> PiecePriority {
        self.inner.read().get(&idx).copied().unwrap_or_default()
    }

    /// Apply a batch of (piece, priority) updates. Setting a piece to
    /// `Normal` removes it from the sparse map. The batch is applied
    /// under a single write lock so concurrent readers see a consistent
    /// view.
    pub(crate) fn set_many(
        &self,
        updates: impl IntoIterator<Item = (ValidPieceIndex, PiecePriority)>,
    ) {
        let mut g = self.inner.write();
        for (idx, prio) in updates {
            match prio {
                PiecePriority::Normal => {
                    g.remove(&idx);
                }
                _ => {
                    g.insert(idx, prio);
                }
            }
        }
    }

    /// Return the set of pieces currently marked at the given tier.
    /// Returns a `Vec` rather than a borrowed iterator so the caller
    /// can drop the read lock before iterating (the scheduler holds
    /// other locks while iterating, and re-acquiring this lock per item
    /// would be wasteful).
    pub(crate) fn iter_tier(&self, tier: PiecePriority) -> Vec<ValidPieceIndex> {
        let g = self.inner.read();
        g.iter()
            .filter(|(_, p)| **p == tier)
            .map(|(idx, _)| *idx)
            .collect()
    }

    /// Snapshot of the current sparse map. Used by
    /// [`crate::ManagedTorrent::piece_priorities`] for external
    /// introspection (debug tools, future `Api` JSON shape).
    pub(crate) fn snapshot(&self) -> HashMap<ValidPieceIndex, PiecePriority> {
        self.inner.read().clone()
    }
}
```

#### 2. `librqbit/src/torrent_state/mod.rs` (lines 1-30 — module list)

Declare the new module alongside the existing siblings.

```diff
 mod initializing;
 pub mod live;
 mod paused;
 pub(crate) mod stats;
 pub(crate) mod streaming;
+pub(crate) mod piece_priorities;
 pub(crate) mod utils;
```

(Adjust to match the exact line numbers in the file; the module-list
block is at the top of `torrent_state/mod.rs` and the existing module
declarations follow this style.)

#### 3. `librqbit/src/lib.rs` (lines 87-91 — re-exports)

Re-export `PiecePriority` from the crate root so external callers can
write `librqbit::PiecePriority` without reaching into the
`torrent_state::piece_priorities` internal path.

```diff
 pub use torrent_state::{
     ManagedTorrent, ManagedTorrentShared, ManagedTorrentState, TorrentMetadata, TorrentStats,
     TorrentStatsState,
+    piece_priorities::PiecePriority,
 };
 pub use type_aliases::FileInfos;
```

`TorrentPiecePriorities` itself is NOT re-exported — it's a
`pub(crate)` storage detail, accessed externally only via
`ManagedTorrent::set_piece_priorities` and
`ManagedTorrent::piece_priorities`.

#### 4. `librqbit/src/torrent_state/live/mod.rs` (lines 176-212 — `TorrentStateLive` struct)

Add the `piece_priorities` field as a sibling to the existing
`streams: Arc<TorrentStreams>` field. Same Arc shape so pause/resume
clones the inner state through naturally.

```diff
 pub struct TorrentStateLive {
     peers: PeerStates,
     shared: Arc<ManagedTorrentShared>,
     metadata: Arc<TorrentMetadata>,
     locked: RwLock<TorrentStateLocked>,
     // … existing fields elided …

     pub(crate) streams: Arc<TorrentStreams>,
+    /// Per-piece priority overrides. Consulted by
+    /// [`Self::reserve_next_needed_piece`] between the per-stream
+    /// lookahead walk and the natural file-priority walk. Preserved
+    /// across pause/resume via the same Arc-clone path as `streams`.
+    pub(crate) piece_priorities: Arc<TorrentPiecePriorities>,
     have_broadcast_tx: tokio::sync::broadcast::Sender<ValidPieceIndex>,
     // … remaining existing fields elided …
 }
```

And in `TorrentStateLive::new()` (lines 214-300), thread the
`piece_priorities` field through the `Arc::new(TorrentStateLive { ... })`
literal, taking the Arc from the paused side:

```diff
 let state = Arc::new(TorrentStateLive {
     shared: paused.shared.clone(),
     metadata: paused.metadata.clone(),
     // … existing peers / locked literals elided …
+    piece_priorities: paused.piece_priorities.clone(),
     streams: paused.streams.clone(),
     // … remaining existing fields elided …
 });
```

And update `TorrentStateLive::pause()` (lines 701-725) so the
priorities Arc rides along into the paused state:

```diff
 Ok(TorrentStatePaused {
     shared: self.shared.clone(),
     metadata: self.metadata.clone(),
     files: self.files.take()?,
     chunk_tracker,
     streams: self.streams.clone(),
+    piece_priorities: self.piece_priorities.clone(),
 })
```

(Imports: add `use crate::torrent_state::piece_priorities::TorrentPiecePriorities;`
to the top of `live/mod.rs`; the `Arc` import is already present.)

#### 5. `librqbit/src/torrent_state/paused.rs` (lines 8-20 — `TorrentStatePaused` struct)

Mirror the same field on the paused side.

```diff
 use super::{streaming::TorrentStreams, ManagedTorrentShared, TorrentMetadata};
+use super::piece_priorities::TorrentPiecePriorities;

 pub(crate) struct TorrentStatePaused {
     pub(crate) shared: Arc<ManagedTorrentShared>,
     pub(crate) metadata: Arc<TorrentMetadata>,
     pub(crate) files: Box<dyn TorrentStorage>,
     pub(crate) chunk_tracker: ChunkTracker,
     pub(crate) streams: Arc<TorrentStreams>,
+    /// Per-piece priority overrides. See [`TorrentPiecePriorities`].
+    /// Created once per torrent in
+    /// [`crate::torrent_state::initializing::TorrentStateInitializing::check`]
+    /// and shared with `TorrentStateLive` via Arc-clone on resume.
+    pub(crate) piece_priorities: Arc<TorrentPiecePriorities>,
 }
```

(Adjust import paths to match the file's existing style — the
`use super::...` line is at the top of the file; the Arc import
already exists since the struct already holds `Arc<TorrentStreams>`.)

#### 6. `librqbit/src/torrent_state/initializing.rs` (lines 272-280 — `TorrentStatePaused` construction)

Initialize the new Arc once per torrent here, mirroring the
`streams: Arc::new(Default::default())` pattern already in place.

```diff
 let paused = TorrentStatePaused {
     shared: self.shared.clone(),
     metadata: self.metadata.clone(),
     files: self.files.take()?,
     chunk_tracker,
     streams: Arc::new(Default::default()),
+    piece_priorities: Arc::new(Default::default()),
 };
```

(Import: add `use super::piece_priorities::TorrentPiecePriorities;` if
the type's Default impl is the only thing referenced — the Arc and
`Default::default()` calls don't need a direct type reference, so the
import is only needed if rustc's type inference flags ambiguity. In
practice the existing `Arc::new(Default::default())` for `streams`
works without an explicit `TorrentStreams` import in this file because
the field's type is fixed on `TorrentStatePaused`; the same applies
here.)

#### 7. `librqbit/src/torrent_state/live/mod.rs` (lines 1227-1276 — `reserve_next_needed_piece`)

The substantive scheduler change. Two new tier walks (HIGHEST and HIGH)
chained between the existing per-stream lookahead walk and the natural
file-priority walk. DontDownload filter applied to all three pre-existing
walk surfaces.

```diff
 fn reserve_next_needed_piece(&self) -> anyhow::Result<Option<ValidPieceIndex>> {
     // TODO: locking one inside the other in different order results in deadlocks.
     self.state
         .peers
         .with_live_mut(self.addr, "reserve_next_needed_piece", |live| {
             if self.locked.read().i_am_choked {
                 debug!("we are choked, can't reserve next piece");
                 return Ok(None);
             }
             let mut g = self.state.lock_write("reserve_next_needed_piece");

             let n = {
                 let mut n_opt = None;
                 let bf = &live.bitfield;
                 let chunk_tracker = g.get_chunks()?;
+                let prio = &self.state.piece_priorities;
                 let priority_streamed_pieces = self
                     .state
                     .streams
                     .iter_next_pieces(&self.state.lengths)
                     .filter(|pid| {
                         !chunk_tracker.is_piece_have(*pid)
                             && !g.inflight_pieces.contains_key(pid)
+                            && prio.get(*pid) != PiecePriority::DontDownload
                     });
+                // NEW: explicit HIGHEST tier from `set_piece_priorities`. Walked
+                // after the per-stream lookahead because streams already imply
+                // HIGHEST for their active windows; callers that need HIGHEST
+                // outside a stream context (e.g. a "download this file fast"
+                // tool) reach the same effect through this tier.
+                let highest_tier = prio
+                    .iter_tier(PiecePriority::Highest)
+                    .into_iter()
+                    .filter(|pid| {
+                        !chunk_tracker.is_piece_have(*pid)
+                            && !g.inflight_pieces.contains_key(pid)
+                    });
+                // NEW: explicit HIGH tier — PRD §F-014's `[pos+60s, pos+300s]`
+                // and last-piece-HIGH map onto this tier. Walked before the
+                // natural file-priority order so these pieces are picked up
+                // earlier than they otherwise would be.
+                let high_tier = prio
+                    .iter_tier(PiecePriority::High)
+                    .into_iter()
+                    .filter(|pid| {
+                        !chunk_tracker.is_piece_have(*pid)
+                            && !g.inflight_pieces.contains_key(pid)
+                    });
                 let natural_order_pieces = chunk_tracker
-                    .iter_queued_pieces(&g.file_priorities, &self.state.metadata.file_infos);
+                    .iter_queued_pieces(&g.file_priorities, &self.state.metadata.file_infos)
+                    .filter(|pid| {
+                        // DontDownload is a hard skip; Highest/High pieces have
+                        // already been yielded in the dedicated tier walks above
+                        // (the inner loop breaks on first match, but filtering
+                        // here keeps the walk cheap when no HIGHEST/HIGH match
+                        // exists on this peer).
+                        let p = prio.get(*pid);
+                        p != PiecePriority::DontDownload
+                            && p != PiecePriority::Highest
+                            && p != PiecePriority::High
+                    });
-                for n in priority_streamed_pieces.chain(natural_order_pieces) {
+                for n in priority_streamed_pieces
+                    .chain(highest_tier)
+                    .chain(high_tier)
+                    .chain(natural_order_pieces)
+                {
                     if bf.get(n.get() as usize).map(|v| *v) == Some(true) {
                         n_opt = Some(n);
                         break;
                     }
                 }

                 match n_opt {
                     Some(n_opt) => n_opt,
                     None => return Ok(None),
                 }
             };
             // … remainder of the function unchanged (inflight insert +
             // reserve_needed_piece call + Ok(Some(n))) …
         })
         .transpose()
         .map(|r| r.flatten())
 }
```

(Imports: add `use crate::torrent_state::piece_priorities::PiecePriority;`
to the top of `live/mod.rs`; the function lives in the same file so a
single use-statement suffices for both the field reference in
`TorrentStateLive` and the enum reference in the walk.)

**Endgame interaction:** the endgame mode kicks in when very few
pieces remain (the existing `try_steal_old_slow_piece` flow at lines
1278-1320). Endgame relies on `inflight_pieces` membership to identify
candidates to steal; the tier walks above only INSERT into
`inflight_pieces` (via `reserve_needed_piece`), they never read it for
endgame purposes. So endgame is structurally unaffected: a piece
that's marked HIGHEST and reserved by peer A can still be stolen by
peer B if A is too slow, regardless of priority annotation.

**Wake-on-mutation flow:** see file 9 below.

#### 8. `librqbit/src/torrent_state/mod.rs` (lines 203-300 — `ManagedTorrent` impl, public API surface)

Add the two new public methods to `ManagedTorrent`. Both reach into
the current `state` to find the live-or-paused `piece_priorities` Arc
and either mutate it (setter) or snapshot it (getter).

```diff
 impl ManagedTorrent {
     pub fn id(&self) -> TorrentId {
         self.shared.id
     }
     // … other existing methods elided …

+    /// Set per-piece scheduling priorities for the given file.
+    ///
+    /// The `ranges` argument carries `(file_piece_range, priority)`
+    /// pairs where `file_piece_range` is over piece indices relative
+    /// to the file's start (NOT torrent-absolute piece indices). The
+    /// method translates these to absolute [`ValidPieceIndex`] values
+    /// via the file's `piece_range` from [`crate::file_info::FileInfo`]
+    /// before applying them to the sparse storage.
+    ///
+    /// The scheduler picks pieces in this tier order: per-stream
+    /// lookahead → `Highest` → `High` → `Normal`. `DontDownload`-marked
+    /// pieces are never reserved, even if an active stream's lookahead
+    /// window covers them.
+    ///
+    /// Setting a piece to [`PiecePriority::Normal`] removes any
+    /// previous explicit annotation (the storage is sparse — `Normal`
+    /// is the implicit default).
+    ///
+    /// # Errors
+    ///
+    /// Returns an error if the torrent's metadata is not yet resolved
+    /// (magnet links pre-info-fetch), if `file_id` is out of range, or
+    /// if any of the supplied ranges falls outside the file's piece
+    /// range.
+    pub fn set_piece_priorities(
+        &self,
+        file_id: usize,
+        ranges: impl IntoIterator<Item = (std::ops::Range<usize>, crate::PiecePriority)>,
+    ) -> anyhow::Result<()> {
+        let metadata = self
+            .metadata
+            .load_full()
+            .context("torrent metadata is not resolved")?;
+        let file_info = metadata
+            .file_infos
+            .get(file_id)
+            .with_context(|| format!("file_id {file_id} out of range"))?;
+        let file_piece_range = file_info.piece_range_usize();
+
+        let priorities_arc = self.with_state(|s| match s {
+            ManagedTorrentState::Live(l) => Ok(l.piece_priorities.clone()),
+            ManagedTorrentState::Paused(p) => Ok(p.piece_priorities.clone()),
+            ManagedTorrentState::Initializing(_) => {
+                bail!("torrent is still initializing; piece priorities cannot be set yet")
+            }
+            ManagedTorrentState::Error(_) => {
+                bail!("torrent is in error state")
+            }
+            ManagedTorrentState::None => bail!("bug: torrent is in empty state"),
+        })?;
+
+        let translated: Vec<(ValidPieceIndex, crate::PiecePriority)> = ranges
+            .into_iter()
+            .flat_map(|(r, prio)| {
+                r.map(move |file_local_piece_id| {
+                    let absolute = file_piece_range.start + file_local_piece_id;
+                    (absolute, prio)
+                })
+            })
+            .filter_map(|(absolute, prio)| {
+                metadata
+                    .lengths
+                    .validate_piece_index(absolute as u32)
+                    .map(|valid| (valid, prio))
+            })
+            .collect();
+
+        priorities_arc.set_many(translated);
+
+        // Wake parked peers so they re-check the queue immediately. The
+        // notify is a no-op if no peers are currently parked.
+        if let Some(live) = self.live() {
+            live.new_pieces_notify.notify_waiters();
+        }
+
+        Ok(())
+    }
+
+    /// Snapshot the current per-piece priority map. Returns an empty
+    /// map if the torrent is not yet initialized or no priorities have
+    /// been set. Useful for debug tools and the `Api` JSON surface.
+    pub fn piece_priorities(
+        &self,
+    ) -> std::collections::HashMap<
+        librqbit_core::lengths::ValidPieceIndex,
+        crate::PiecePriority,
+    > {
+        self.with_state(|s| match s {
+            ManagedTorrentState::Live(l) => l.piece_priorities.snapshot(),
+            ManagedTorrentState::Paused(p) => p.piece_priorities.snapshot(),
+            _ => Default::default(),
+        })
+    }
 }
```

(Imports: add `use librqbit_core::lengths::ValidPieceIndex;` and
`use anyhow::{bail, Context};` to the existing import block in
`torrent_state/mod.rs` if not already present. `anyhow::Context` is
already in use in this file via `with_metadata`; `bail!` is the
macro form.)

#### 9. Wake-on-mutation: `notify_waiters()` semantics

The existing field `new_pieces_notify: tokio::sync::Notify` on
`TorrentStateLive` (line 196) is the wake channel parked peers wait on
when they have no piece to download (see the existing call sites at
lines 1139 and 1791). Calling `notify_waiters()` after a
priority update wakes ALL parked peers; each one re-runs
`reserve_next_needed_piece`, which now sees the new tier annotations.

This is the same notify mechanism used by:
- `mark_piece_downloaded` (line 1139 — when a piece completes, parked
  peers should re-check in case the next piece is now available)
- `update_only_files`'s downstream effects (line 1791 — when the
  selected file set changes, parked peers should re-check)

Adding `set_piece_priorities` as a third notify caller is consistent
with the existing pattern. No new field, no new channel, no new lock.

### Tests

Three `#[tokio::test(flavor = "multi_thread")]` test stubs added to
`librqbit/src/tests/e2e_stream.rs`, following the existing
`e2e_stream()` helper's fixture pattern (dual-session
seeder→leecher + temp-dir random-content torrent + `wait_until_completed`
timeout). Each stub asserts a distinct property of the priority API:

```rust
// Append to librqbit/src/tests/e2e_stream.rs after the existing
// `test_e2e_stream()` function.

use crate::PiecePriority;
use std::time::Duration;

/// Helper: spin up a seeder→leecher pair just like `e2e_stream()` but
/// expose the client's `Arc<ManagedTorrent>` to the caller so they can
/// poke `set_piece_priorities` mid-download.
///
/// Returns once the leecher torrent has finished initializing (chunk
/// tracker built, piece bitfield empty). The caller is responsible for
/// waiting for completion via the returned handle.
async fn e2e_with_priorities<F>(
    file_count: usize,
    file_size: usize,
    inspect: F,
) -> anyhow::Result<()>
where
    F: FnOnce(
            std::sync::Arc<crate::ManagedTorrent>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>,
        > + Send,
{
    // TODO: factor out the dual-session fixture currently inlined in
    // `e2e_stream()` (lines 16-105). The helper should yield the
    // `client_handle: Arc<ManagedTorrent>` after `wait_until_initialized`
    // so the caller's `inspect` closure runs against a live torrent
    // with no pieces downloaded yet. Once factored, this body becomes
    // ~15 lines of setup + `inspect(client_handle).await?` +
    // `client_handle.wait_until_completed().await?`.
    todo!("extract fixture from existing e2e_stream() body");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_piece_priorities_highest_pieces_selected_first() -> anyhow::Result<()> {
    use tokio::time::timeout;
    // 4 files × 32 KiB each; the torrent has ~128 pieces at the
    // 1024-byte piece_length the existing fixture uses.
    timeout(Duration::from_secs(15), e2e_with_priorities(4, 32 * 1024, |handle| {
        Box::pin(async move {
            // Mark the LAST piece of file 0 as HIGHEST. The natural
            // file-priority walk would fetch it after every preceding
            // file 0 piece; the HIGHEST tier should pull it forward.
            let metadata = handle.metadata.load_full().unwrap();
            let file_0_piece_count = metadata.file_infos[0].piece_range.len();
            handle.set_piece_priorities(
                0,
                std::iter::once((file_0_piece_count - 1..file_0_piece_count, PiecePriority::Highest)),
            )?;

            // Wait until the leecher has at least 1 piece. Then check
            // that the last piece of file 0 is among the have-set.
            //
            // TODO: poll `handle.stats().progress.have_bytes` until it
            // exceeds the first-piece size (1024 B); then snapshot
            // `with_chunk_tracker(|ct| ct.get_have_pieces().clone())`
            // and assert the last-piece bit is set.
            todo!("wait + assert have-bit on last piece of file 0");
        })
    })).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_piece_priorities_dont_download_excluded() -> anyhow::Result<()> {
    use tokio::time::timeout;
    // 2 files × 4 KiB each; file 1 marked entirely DontDownload.
    // After completion, file 1's piece-have bits should remain 0.
    timeout(Duration::from_secs(15), e2e_with_priorities(2, 4 * 1024, |handle| {
        Box::pin(async move {
            let metadata = handle.metadata.load_full().unwrap();
            let file_1_piece_count = metadata.file_infos[1].piece_range.len();
            handle.set_piece_priorities(
                1,
                std::iter::once((0..file_1_piece_count, PiecePriority::DontDownload)),
            )?;

            // TODO: wait until file 0 fully downloaded
            // (`stats().progress.have_bytes >= file_infos[0].len`),
            // then assert file 1's piece-bits are all 0. Note that
            // `wait_until_completed()` will hang because the leecher
            // never reaches `is_finished()` if file 1 is excluded —
            // use a manual completion poll on file 0's byte count
            // instead.
            todo!("poll file 0 completion + assert file 1 have-bits unset");
        })
    })).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_piece_priorities_survive_pause_resume() -> anyhow::Result<()> {
    use tokio::time::timeout;
    // 2 files × 4 KiB each; set HIGH priorities, pause, resume,
    // verify the priorities are still in effect by snapshotting via
    // `piece_priorities()` accessor.
    timeout(Duration::from_secs(20), e2e_with_priorities(2, 4 * 1024, |handle| {
        Box::pin(async move {
            let metadata = handle.metadata.load_full().unwrap();
            let file_0_piece_count = metadata.file_infos[0].piece_range.len();
            handle.set_piece_priorities(
                0,
                std::iter::once((0..file_0_piece_count, PiecePriority::High)),
            )?;

            let before_pause = handle.piece_priorities();
            assert_eq!(
                before_pause.len(),
                file_0_piece_count,
                "all file-0 pieces marked HIGH should be in the snapshot",
            );

            // TODO: pause via `handle.pause()`, then resume via
            // `handle.start(None, false)`. Re-snapshot
            // `handle.piece_priorities()` and assert equality with
            // `before_pause` — the Arc-clone path through
            // `TorrentStatePaused.piece_priorities` should preserve
            // the map verbatim.
            todo!("pause + resume + assert priorities preserved");
        })
    })).await??;
    Ok(())
}
```

The `todo!()` markers are deliberate — extracting the dual-session
fixture from `e2e_stream()` is a refactor the test author needs to do
once and reuse across all three new tests. The assertions themselves
are concrete (specific piece-count comparisons, specific have-bit
checks) and don't need further design work.

A `pub(crate)` accessor on `TorrentStateLive::piece_priorities` may
need to be exposed for the snapshot assertion in
`test_piece_priorities_survive_pause_resume` — the existing
`Arc<TorrentPiecePriorities>` field is already `pub(crate)` per the
file 4 diff above, and the in-crate test can read it directly. If the
test moves to a separate `tests/` integration directory in a follow-up,
add a `#[cfg(test)] pub fn piece_priorities_arc(&self) -> Arc<TorrentPiecePriorities>`
accessor.

### PR description (paste verbatim into the upstream PR for B2)

## Summary

Adds a public `PiecePriority` enum and a setter method
`ManagedTorrent::set_piece_priorities(file_id, ranges)` so downstream
applications can override the scheduler's per-piece selection.

The reservation loop in `TorrentStateLive::reserve_next_needed_piece`
gains two new walk tiers (`Highest` then `High`) inserted between the
existing per-stream lookahead and the natural file-priority walk.
`DontDownload`-marked pieces are filtered out of all walks and never
reserved.

The new state lives in a sparse `HashMap<ValidPieceIndex, PiecePriority>`
inside a sibling `Arc<TorrentPiecePriorities>` field on
`TorrentStateLive` and `TorrentStatePaused`, parallel to the existing
`Arc<TorrentStreams>`. Created once per torrent in
`TorrentStateInitializing::check()`; preserved across pause/resume via
the same Arc-clone mechanism `streams` already uses.

## Motivation

Three downstream use cases:

1. **Adaptive-buffer streaming clients** that want to express
   "fetch the next 60s of file bitrate at HIGHEST, the next 240s at
   HIGH, the last piece of the active file at HIGH". The existing
   per-stream lookahead handles HIGHEST (as of PR B1); the new tiers
   handle the rest.

2. **"Skip episode" / "skip bonus disc" UI affordances** that want
   to remove specific files from the download set without going
   through `update_only_files` (which atomically rewrites the whole
   selected set and is heavier than a per-piece annotation).

3. **".mp4 with trailing `moov` atom"** — set the last piece of the
   active file to HIGH at stream open so the player has the
   moov-atom-bearing piece before it tries to seek.

## Surface area

- New `pub enum PiecePriority { Highest, High, Normal, DontDownload }`
  in a new module `torrent_state/piece_priorities.rs`, re-exported
  from `lib.rs`.
- New `pub(crate) struct TorrentPiecePriorities` in the same module,
  with sparse RwLock-backed `HashMap<ValidPieceIndex, PiecePriority>`
  storage.
- New `Arc<TorrentPiecePriorities>` field on each of
  `TorrentStateLive` and `TorrentStatePaused`, initialized once per
  torrent in `TorrentStateInitializing::check()`.
- New `pub fn ManagedTorrent::set_piece_priorities(file_id, ranges)`
  and `pub fn ManagedTorrent::piece_priorities()` accessor.
- Updated `TorrentStateLive::reserve_next_needed_piece`: two new
  tier walks chained into the existing
  `priority_streamed_pieces.chain(natural_order_pieces)` pattern;
  DontDownload filter added to all three walk surfaces.

## Tests

Three `#[tokio::test(flavor = "multi_thread")]` test stubs in
`librqbit/src/tests/e2e_stream.rs`:

- `test_piece_priorities_highest_pieces_selected_first` — verifies
  a piece marked `Highest` is fetched ahead of the natural
  file-priority order.
- `test_piece_priorities_dont_download_excluded` — verifies pieces
  marked `DontDownload` are never reserved, and the torrent
  intentionally never reaches `is_finished()` for the excluded
  pieces.
- `test_piece_priorities_survive_pause_resume` — verifies the
  priority map survives a pause/resume cycle via the same Arc-clone
  path `streams` already uses.

The three stubs share a new `e2e_with_priorities()` helper that
extracts the existing `e2e_stream()` body's seeder→leecher fixture
into a reusable form. The extraction is a one-time refactor
(~80 lines of currently-inline scaffolding).

## Backwards compatibility

Zero behavior change for callers that never call
`set_piece_priorities`: the default `TorrentPiecePriorities` is
empty, all tier walks yield nothing, the DontDownload filter degrades
to a constant `true`. The existing
`chunk_tracker::iter_queued_pieces` signature is unchanged. The
existing `TorrentStreams::iter_next_pieces` signature is unchanged.
`Session::add_torrent`'s public surface is unchanged.

The new `Arc<TorrentPiecePriorities>` field on `TorrentStateLive`
and `TorrentStatePaused` is `pub(crate)` so external callers don't
need to construct it; the empty default is built automatically
during `TorrentStateInitializing::check()`.

## Interaction with existing scheduler features

- **Endgame mode** (`try_steal_old_slow_piece`, lines 1278-1320):
  unaffected. Endgame steals based on `inflight_pieces` membership,
  which the tier walks insert into via `reserve_needed_piece` after
  selection. A HIGHEST piece reserved by a slow peer A can still be
  stolen by faster peer B regardless of priority annotation.

- **Streams' wake-on-piece-completed**
  (`TorrentStreams::wake_streams_on_piece_completed`, lines 102-119):
  unaffected. Per-stream wakers fire independently of the priority
  storage; HIGH pieces don't have wakers (they're not tied to a
  read position), only stream lookahead pieces do.

- **`update_only_files`** (lines 558-587 in `torrent_state/mod.rs`):
  unchanged signature. `DontDownload` per-piece annotations and
  `update_only_files`-driven file deselection are independent
  mechanisms; both filter the scheduler's walk, but neither one
  overrides the other. A piece in a selected file marked
  `DontDownload` is still skipped; a piece in an unselected file
  marked `Highest` is still skipped (the file isn't in the
  `iter_queued_pieces` walk).

## Future work (not in this PR)

- **`Last-piece-HIGH` convenience**: rather than have every caller
  duplicate `set_piece_priorities(file_id, [(N-1..N, High)])` at
  stream open, a `set_last_piece_high(file_id)` method on
  `ManagedTorrent` could be added if usage shows the boilerplate
  is repetitive. Deliberately left out of this PR to keep the
  surface minimal.

- **`PiecePriority` accessor on `FileStream`**: the existing
  streaming `FileStream` could expose `set_piece_priorities` as
  a method delegating to its owning `ManagedTorrent`. Reasonable
  follow-up; left out of this PR because the `ManagedTorrent`
  reference is already exposed via `FileStream.torrent` (line 132
  of `torrent_state/streaming.rs`).

- **`HTTP API` JSON shape**: the existing `Api` (re-exported from
  `lib.rs:77`) could surface `piece_priorities()` and a
  POST-shaped `set_piece_priorities` endpoint. Worth doing once
  this PR lands and downstream usage materialises.

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
2. New module `crates/kino-torrent/src/piece_priority.rs` exposing
   `update_for_position(handle: &ManagedTorrent, file_id: usize,
   position_s: f64, file_bitrate_bps: f64)` which:
   - Computes the HIGHEST piece range covering
     `[position_s, position_s + PIECE_PRIORITY_HIGH_WINDOW_S]`
     (60s per PRD §F-014) via `file_bitrate_bps` and the file's
     piece length.
   - Computes the HIGH piece range covering
     `[position_s + PIECE_PRIORITY_HIGH_WINDOW_S,
     position_s + PIECE_PRIORITY_MED_WINDOW_S]`
     (60s..300s per PRD §F-014).
   - Adds the file's final piece (`file_info.piece_range.end - 1`)
     to the HIGH set.
   - Diffs against the previous call's range so pieces that are no
     longer in any tier are explicitly demoted to `Normal` (otherwise
     stale `Highest`/`High` annotations would accumulate as the
     playhead advances).
   - Calls `handle.set_piece_priorities(file_id, ranges)` with the
     three computed ranges (HIGHEST / HIGH / NORMAL-demotions).

   Note: with PR B1 already wired, the HIGHEST tier is redundant
   here for the active stream (the stream's lookahead already
   covers it), but explicit annotation costs nothing and helps when
   the user pauses playback for a long time (the stream's lookahead
   drains, but the explicit HIGHEST keeps the scheduler honest).

3. `crates/kino-torrent/src/monitor.rs::BufferMonitor` calls the new
   function from its existing 1-second sampler tick (PRD §F-014
   `AHEAD_CHECK_INTERVAL_MS = 1000` for sampling; the 5-second
   recompute `RECOMPUTE_INTERVAL_S` is too coarse for HIGH-tier
   responsiveness on real-world torrents).

4. New unit tests in `crates/kino-torrent/tests/buffer_monitor.rs`:
   - HIGHEST/HIGH ranges computed correctly for representative
     position/bitrate combinations (4K 80 Mbps, 1080p 8 Mbps,
     audio-only 320 kbps).
   - Stale-annotation demotion: advancing playhead by `>300s`
     between calls produces a NORMAL demotion for the previously-
     HIGH pieces that have fallen out of the window.
   - Last-piece-HIGH is always included regardless of playhead
     position.

5. F-014 §6A entry in `STATE.md` flips to RESOLVED.

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
  of the crate's stable surface. PR B2's new `PiecePriority` enum
  and `set_piece_priorities` / `piece_priorities` methods similarly
  need full `///` docs — they're the user-facing API for the whole
  feature.
- **PR B2 sequencing on the kino side.** The post-merge kino-side
  wiring for B2 (the new
  `crates/kino-torrent/src/piece_priority.rs` module +
  `BufferMonitor` integration) DEPENDS on B1's wiring being in
  place — both because B1 introduces the `LOOKAHEAD_MIN_BYTES` /
  `LOOKAHEAD_MAX_BYTES` constants the B2 wiring also reads for the
  stream-lookahead approximation, and because the B2 monitor loop
  feeds `set_piece_priorities` calls that complement (don't
  replace) B1's lookahead-driven HIGHEST coverage. A kino session
  that wires B2 without B1's wiring already on `main` would leave
  the HIGHEST tier permanently empty (no caller annotates it), so
  the §6A entry would not flip to RESOLVED until B1 was also
  wired.
