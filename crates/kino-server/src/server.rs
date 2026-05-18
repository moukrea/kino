//! Local HTTP server + token registry.
//!
//! `Server::spawn` binds on `127.0.0.1:0`, returns a [`ServerHandle`] that
//! owns the bound address and the (clonable) session registry. The handle
//! is what the Tauri host stashes in managed state; per-playback sessions
//! are registered and unregistered by token.
//!
//! ## Why no per-route auth
//!
//! The bind address is `127.0.0.1`, so off-host connections can't reach
//! it. UUID-v4 tokens are unguessable in practice; the only privacy
//! threat is a local process sniffing localhost, which is out of scope
//! for v1 (PRD §3 "process model" treats the host as trusted).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use bytes::Bytes;
use futures::Stream;
use kino_torrent::AddedTorrent;
use parking_lot::RwLock;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncSeekExt, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::range::{parse as parse_range, RangeParse};

/// Streaming chunk size (64 KiB). Picked to match libmpv's default cache
/// granularity; small enough to keep per-request memory bounded, large
/// enough to amortize syscall + librqbit-piece overhead.
const STREAM_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("could not bind 127.0.0.1:0: {0}")]
    Bind(std::io::Error),
    #[error("server task failed: {0}")]
    Task(String),
}

/// One registered playback session — a binding from a token to a specific
/// file inside a specific torrent.
#[derive(Clone)]
pub struct StreamSession {
    /// Stable random identifier; appears in the URL as the path segment.
    pub token: Uuid,
    /// Live torrent handle returned by the engine.
    pub torrent: AddedTorrent,
    /// Index into `torrent.files()` selecting the playable file.
    pub file_index: usize,
    /// File size in bytes (cached so the route doesn't re-walk the file
    /// list on every Range request).
    pub file_size: u64,
    /// Display name of the file (used for Content-Disposition).
    pub file_name: String,
    /// Resolved MIME type (best-effort from the file extension).
    pub mime: String,
}

impl std::fmt::Debug for StreamSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSession")
            .field("token", &self.token)
            .field("file_index", &self.file_index)
            .field("file_size", &self.file_size)
            .field("file_name", &self.file_name)
            .field("mime", &self.mime)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
struct Registry {
    sessions: Arc<RwLock<HashMap<Uuid, StreamSession>>>,
}

impl Registry {
    fn insert(&self, s: StreamSession) {
        self.sessions.write().insert(s.token, s);
    }
    fn remove(&self, token: Uuid) -> Option<StreamSession> {
        self.sessions.write().remove(&token)
    }
    fn get(&self, token: Uuid) -> Option<StreamSession> {
        self.sessions.read().get(&token).cloned()
    }
}

/// Handle to the running server. Cloneable (cheap, internally `Arc`'d).
/// Dropping every clone keeps the server alive — call [`Self::shutdown`]
/// to stop it before app exit.
#[derive(Clone)]
pub struct ServerHandle {
    addr: SocketAddr,
    registry: Registry,
    shutdown: Arc<parking_lot::Mutex<Option<oneshot::Sender<()>>>>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("addr", &self.addr)
            .field("sessions", &self.registry.sessions.read().len())
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Spawn a new server on `127.0.0.1:0`. Returns once the listener is
    /// bound and the OS-assigned port is known.
    pub async fn spawn() -> Result<Self, ServerError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(ServerError::Bind)?;
        let addr = listener.local_addr().map_err(ServerError::Bind)?;
        let registry = Registry::default();
        let app = router(registry.clone());
        let (tx, rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = server.await {
                tracing::error!(error = %e, "kino-server task exited with error");
            }
        });

        Ok(Self {
            addr,
            registry,
            shutdown: Arc::new(parking_lot::Mutex::new(Some(tx))),
        })
    }

    /// Bound address, e.g. `127.0.0.1:46327`. Use [`Self::base_url`] for
    /// the URL prefix passed to the player.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:{port}` (no trailing slash).
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `http://127.0.0.1:{port}/stream/{token}` — what the host passes to
    /// the player.
    #[must_use]
    pub fn stream_url(&self, token: Uuid) -> String {
        format!("http://{}/stream/{}", self.addr, token)
    }

    /// Register a new session. Build the [`StreamSession`] from an
    /// [`AddedTorrent`] + the selected file index; this helper resolves
    /// name/size/MIME for you.
    ///
    /// Returns the token (random UUID v4 minted by this call).
    pub fn register(&self, torrent: AddedTorrent, file_index: usize) -> Result<Uuid, ServerError> {
        let file_size = torrent
            .file_size(file_index)
            .ok_or_else(|| ServerError::Task(format!("invalid file_index {file_index}")))?;
        let file_name = torrent
            .file_name(file_index)
            .ok_or_else(|| ServerError::Task(format!("invalid file_index {file_index}")))?
            .to_string();
        let mime = mime_guess::from_path(&file_name)
            .first_or_octet_stream()
            .essence_str()
            .to_string();

        let token = Uuid::new_v4();
        self.registry.insert(StreamSession {
            token,
            torrent,
            file_index,
            file_size,
            file_name,
            mime,
        });
        Ok(token)
    }

    /// Unregister a session by token. Returns the removed session if
    /// present.
    pub fn unregister(&self, token: Uuid) -> Option<StreamSession> {
        self.registry.remove(token)
    }

    /// Snapshot of a session by token. Useful for `playback_status` in the
    /// Tauri host.
    #[must_use]
    pub fn session(&self, token: Uuid) -> Option<StreamSession> {
        self.registry.get(token)
    }

    /// Number of currently registered sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.registry.sessions.read().len()
    }

    /// Graceful shutdown. Safe to call repeatedly; only the first call
    /// signals the underlying task.
    pub fn shutdown(&self) {
        if let Some(tx) = self.shutdown.lock().take() {
            let _ = tx.send(());
        }
    }
}

fn router(registry: Registry) -> Router {
    Router::new()
        .route("/stream/:token", get(stream_handler).head(stream_handler))
        .route("/stream/:token", any(method_not_allowed))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(registry)
}

#[allow(clippy::unused_async)]
async fn method_not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response()
}

async fn stream_handler(
    State(registry): State<Registry>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Ok(uuid) = Uuid::parse_str(&token) else {
        return (StatusCode::NOT_FOUND, "unknown stream").into_response();
    };
    let Some(session) = registry.get(uuid) else {
        return (StatusCode::NOT_FOUND, "unknown stream").into_response();
    };

    let range_hdr = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());

    match parse_range(range_hdr, session.file_size) {
        RangeParse::Full => serve_full(&session, method == Method::HEAD),
        RangeParse::Single(s) => serve_range(&session, s.start, s.end, method == Method::HEAD),
        RangeParse::Unsatisfiable => {
            let mut resp =
                (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
            let h = resp.headers_mut();
            h.insert(
                axum::http::header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", session.file_size))
                    .expect("valid header"),
            );
            h.insert(
                axum::http::header::ACCEPT_RANGES,
                HeaderValue::from_static("bytes"),
            );
            resp
        }
    }
}

fn serve_full(session: &StreamSession, head_only: bool) -> Response {
    let mut headers = base_headers(session);
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from_str(&session.file_size.to_string()).expect("valid header"),
    );

    if head_only {
        return (StatusCode::OK, headers, Body::empty()).into_response();
    }

    let stream = match session.torrent.open_stream(session.file_index) {
        Ok(s) => s,
        Err(e) => return engine_error_response(&e),
    };
    let body = Body::from_stream(ChunkStream::new(stream, 0, session.file_size));
    (StatusCode::OK, headers, body).into_response()
}

fn serve_range(session: &StreamSession, start: u64, end: u64, head_only: bool) -> Response {
    let content_length = end - start + 1;
    let mut headers = base_headers(session);
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string()).expect("valid header"),
    );
    headers.insert(
        axum::http::header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{}", session.file_size))
            .expect("valid header"),
    );

    if head_only {
        return (StatusCode::PARTIAL_CONTENT, headers, Body::empty()).into_response();
    }

    let stream = match session.torrent.open_stream(session.file_index) {
        Ok(s) => s,
        Err(e) => return engine_error_response(&e),
    };
    let body = Body::from_stream(ChunkStream::new(stream, start, content_length));
    (StatusCode::PARTIAL_CONTENT, headers, body).into_response()
}

fn base_headers(session: &StreamSession) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_str(&session.mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    // Discourage proxies/caches from buffering or transforming the stream.
    h.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    h
}

fn engine_error_response(err: &kino_torrent::EngineError) -> Response {
    tracing::warn!(error = %err, "open_stream failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("stream unavailable: {err}"),
    )
        .into_response()
}

/// Body adapter: reads from a librqbit stream with seeking, emits 64 KiB
/// chunks until `remaining` bytes have been forwarded or EOF is hit.
///
/// Why hand-rolled instead of `axum_range`/`tokio_util::io::ReaderStream`:
///
/// - The librqbit `FileStream` is opaque (private return type of
///   `ManagedTorrent::stream`), so we wrap it as `Box<dyn FileStream>`
///   ourselves (see `kino_torrent::FileStream`).
/// - We need a single-source-of-truth chunk size that matches the
///   adaptive-buffer scheduler's tick. `ReaderStream` defaults to 8 KiB
///   which doubles the per-request syscall count at no benefit.
struct ChunkStream {
    inner: Box<dyn kino_torrent::FileStream>,
    /// Bytes still owed to the client. Decrements as `poll_next` yields
    /// chunks; the stream ends when this reaches `0`.
    remaining: u64,
    /// First read may require a seek to `start_offset` before any bytes
    /// can be emitted. `None` once the seek has completed.
    pending_seek: Option<u64>,
}

impl ChunkStream {
    fn new(
        inner: Box<dyn kino_torrent::FileStream>,
        start_offset: u64,
        content_length: u64,
    ) -> Self {
        Self {
            inner,
            remaining: content_length,
            pending_seek: Some(start_offset),
        }
    }
}

impl Stream for ChunkStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }

        // Resolve any pending seek before reading. `start_seek` is sync;
        // `poll_complete` drives the seek to completion. Both must agree
        // on the target offset.
        if let Some(target) = self.pending_seek {
            let inner = Pin::new(&mut *self.inner);
            // Use the AsyncSeekExt::seek(SeekFrom::Start(...)) path via raw
            // start_seek/poll_complete to integrate with poll_next.
            if let Err(e) = inner.start_seek(std::io::SeekFrom::Start(target)) {
                return Poll::Ready(Some(Err(e)));
            }
            self.pending_seek = None;
        }

        // Always drive any in-flight seek to completion. After a fresh
        // start_seek above, this is also where we wait for the seek to
        // land before the first read.
        {
            let inner = Pin::new(&mut *self.inner);
            match inner.poll_complete(cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
                Poll::Pending => return Poll::Pending,
            }
        }

        // The `min` clamps to `STREAM_CHUNK_SIZE` (a `usize`), so the cast
        // back to `usize` cannot truncate on any target. Asserted by
        // `usize::try_from(...).unwrap_or(STREAM_CHUNK_SIZE)`.
        let to_read = usize::try_from(std::cmp::min(self.remaining, STREAM_CHUNK_SIZE as u64))
            .unwrap_or(STREAM_CHUNK_SIZE);
        let mut buf = vec![0u8; to_read];
        let mut read_buf = ReadBuf::new(&mut buf);

        let n = {
            let inner = Pin::new(&mut *self.inner);
            match inner.poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) => read_buf.filled().len(),
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
                Poll::Pending => return Poll::Pending,
            }
        };

        if n == 0 {
            // Unexpected EOF before we satisfied the requested range.
            // Treat as end-of-body — the client will see a short response
            // and the player will surface an error.
            return Poll::Ready(None);
        }
        buf.truncate(n);
        self.remaining -= n as u64;
        Poll::Ready(Some(Ok(Bytes::from(buf))))
    }
}

// Helper trait bridge: we use AsyncSeekExt::seek (combinator form) in tests
// but the stream adapter above only needs start_seek/poll_complete.
// Keeping AsyncSeekExt in scope here so future polishing can use the
// higher-level combinators if we move to a tokio_util ReaderStream.
#[allow(dead_code)]
fn _ensure_traits_in_scope() {
    fn _take<R: AsyncRead + AsyncSeekExt>(_: R) {}
}
