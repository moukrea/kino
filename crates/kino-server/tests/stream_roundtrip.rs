//! PRD §F-013 integration test:
//!
//! > "Integration test: feed a known torrent fixture, stream it, verify
//! >  byte-for-byte over HTTP."
//!
//! We do this end-to-end with the real librqbit engine, but offline: a
//! synthetic 1 MiB random fixture is written to disk, librqbit creates a
//! torrent for it, kino's engine adds the torrent and hash-checks the
//! existing bytes (no peers needed). The kino-server route then serves the
//! file over HTTP and we compare bytes.
//!
//! Additionally we exercise Range semantics PRD §F-013 calls out:
//!
//! - `Range: bytes=0-` → full body, 206
//! - `Range: bytes=512-1023` → 512 bytes, 206
//! - `Range: bytes=-1024` → suffix range, 206
//! - `Range: bytes=999999999-` → 416 (unsatisfiable)
//! - `HEAD` returns Content-Length without a body
//!
//! Why offline matters: CI runners have no torrent peers; the test must
//! make zero outbound connections. The engine is built with DHT disabled
//! and zero supplementary trackers; the torrent metainfo has no announce
//! URLs. Hash-checking the on-disk pieces is all that needs to happen.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use kino_server::ServerHandle;
use kino_torrent::{AddInput, Engine, EngineConfig};
use rand::{RngCore, SeedableRng};

const FIXTURE_SIZE: usize = 1024 * 1024; // 1 MiB

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn end_to_end_byte_for_byte_over_http() {
    let (engine, fixture_bytes, torrent_bytes, _data_dir) = build_seeded_engine().await;

    let added = engine
        .add(AddInput::Bytes(torrent_bytes))
        .await
        .expect("add torrent");
    assert_eq!(added.files().len(), 1, "single-file torrent");
    assert_eq!(added.files()[0].size, FIXTURE_SIZE as u64);

    let server = ServerHandle::spawn().await.expect("spawn server");
    let token = server.register(added, 0).expect("register session");
    let url = server.stream_url(token);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // 1) Full GET should match every byte.
    let resp = client.get(&url).send().await.expect("full GET");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes"
    );
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .unwrap()
            .to_str()
            .unwrap(),
        &FIXTURE_SIZE.to_string()
    );
    let body = resp.bytes().await.expect("body").to_vec();
    assert_eq!(body.len(), FIXTURE_SIZE, "full body length");
    assert_eq!(body, fixture_bytes, "full body content matches fixture");

    // 2) Closed range `Range: bytes=512-1023` → 512 bytes, 206.
    let resp = client
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=512-1023")
        .send()
        .await
        .expect("ranged GET");
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .unwrap()
            .to_str()
            .unwrap(),
        &format!("bytes 512-1023/{FIXTURE_SIZE}")
    );
    let body = resp.bytes().await.expect("body").to_vec();
    assert_eq!(body.len(), 512);
    assert_eq!(body, fixture_bytes[512..=1023]);

    // 3) Suffix range `Range: bytes=-1024` → last KiB.
    let resp = client
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=-1024")
        .send()
        .await
        .expect("suffix GET");
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    let body = resp.bytes().await.expect("body").to_vec();
    assert_eq!(body.len(), 1024);
    assert_eq!(body, fixture_bytes[FIXTURE_SIZE - 1024..]);

    // 4) Open-ended range `Range: bytes=N-` mid-file.
    let mid_start = FIXTURE_SIZE / 2;
    let resp = client
        .get(&url)
        .header(reqwest::header::RANGE, format!("bytes={mid_start}-"))
        .send()
        .await
        .expect("open-ended GET");
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    let body = resp.bytes().await.expect("body").to_vec();
    assert_eq!(body.len(), FIXTURE_SIZE - mid_start);
    assert_eq!(body, fixture_bytes[mid_start..]);

    // 5) Unsatisfiable range → 416 with Content-Range: bytes */N.
    let resp = client
        .get(&url)
        .header(reqwest::header::RANGE, "bytes=999999999-")
        .send()
        .await
        .expect("416 GET");
    assert_eq!(resp.status(), reqwest::StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .unwrap()
            .to_str()
            .unwrap(),
        &format!("bytes */{FIXTURE_SIZE}")
    );

    // 6) HEAD returns Content-Length, Accept-Ranges, no body.
    let resp = client.head(&url).send().await.expect("HEAD");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .unwrap()
            .to_str()
            .unwrap(),
        &FIXTURE_SIZE.to_string()
    );
    assert_eq!(
        resp.headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes"
    );
    let body = resp.bytes().await.expect("head body").to_vec();
    assert!(body.is_empty(), "HEAD body must be empty");

    // 7) Unknown token → 404.
    let unknown_url = format!("{}/stream/{}", server.base_url(), uuid::Uuid::new_v4());
    let resp = client.get(&unknown_url).send().await.expect("unknown");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // Cleanup.
    let removed = server.unregister(token);
    assert!(removed.is_some(), "session was registered");
    assert_eq!(server.session_count(), 0);
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_ranged_reads_do_not_corrupt_each_other() {
    // PRD §F-013 acceptance: "Range requests work; player seek does not
    // break the scheduler." The player issues many overlapping ranges as
    // the user scrubs. This test fires several concurrently and checks
    // every byte.
    let (engine, fixture_bytes, torrent_bytes, _data_dir) = build_seeded_engine().await;
    let added = engine.add(AddInput::Bytes(torrent_bytes)).await.unwrap();
    let server = ServerHandle::spawn().await.unwrap();
    let token = server.register(added, 0).unwrap();
    let url = server.stream_url(token);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // Five overlapping ranges, fired concurrently.
    let total = FIXTURE_SIZE as u64;
    let ranges: Vec<(u64, u64)> = vec![
        (0, 65_535),
        (1_024, 8_191),
        (total / 2, total / 2 + 4_095),
        (total - 1_024, total - 1),
        (0, total - 1),
    ];

    let mut handles = Vec::new();
    for (start, end) in ranges {
        let client = client.clone();
        let url = url.clone();
        let fixture_bytes = fixture_bytes.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .get(&url)
                .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);
            let body = resp.bytes().await.unwrap().to_vec();
            let expected_len = usize::try_from(end - start + 1).unwrap();
            assert_eq!(body.len(), expected_len, "range {start}-{end} length");
            let start_us = usize::try_from(start).unwrap();
            let end_us = usize::try_from(end).unwrap();
            assert_eq!(
                body.as_slice(),
                &fixture_bytes[start_us..=end_us],
                "range {start}-{end} bytes"
            );
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    server.shutdown();
}

// ---- helpers ---------------------------------------------------------------

/// Build a librqbit `Engine` pointed at a tmp directory containing a known
/// random fixture and return the engine plus the bytes/torrent that
/// describe it. The torrent has no announce URLs, so adding it doesn't
/// trigger any network I/O.
async fn build_seeded_engine() -> (Engine, Vec<u8>, bytes::Bytes, tempfile::TempDir) {
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_path = data_dir.path().join("data.bin");

    // Deterministic content so failures are reproducible.
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(0xF013_F013_F013_F013);
    let mut fixture_bytes = vec![0u8; FIXTURE_SIZE];
    rng.fill_bytes(&mut fixture_bytes);
    {
        let mut f = std::fs::File::create(&fixture_path).expect("create fixture");
        f.write_all(&fixture_bytes).expect("write fixture");
        f.sync_all().expect("sync fixture");
    }

    let opts = librqbit::CreateTorrentOptions {
        name: Some("data.bin"),
        // Smaller piece length so the test runs quickly with a 1 MiB
        // fixture (default is 2 MiB which would be one piece).
        piece_length: Some(64 * 1024),
    };
    let result = librqbit::create_torrent(&fixture_path, opts)
        .await
        .expect("create torrent");
    let torrent_bytes = result.as_bytes().expect("bencode torrent");

    let cache_root: PathBuf = data_dir.path().to_path_buf();
    let cfg = EngineConfig {
        cache_root,
        enable_dht: false,
        enable_pex: false,
        enable_lsd: false,
        supplementary_trackers: Vec::new(),
        init_timeout: Duration::from_secs(30),
    };
    let engine = Engine::new(cfg).await.expect("engine");

    (engine, fixture_bytes, torrent_bytes, data_dir)
}
