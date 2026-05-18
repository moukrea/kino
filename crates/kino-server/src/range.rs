//! HTTP/1.1 Range header parsing (RFC 7233) — single-range subset.
//!
//! kino's player issues at most one range per request; we don't bother
//! supporting `multipart/byteranges` responses. The parser surfaces three
//! outcomes:
//!
//! - [`RangeParse::Full`]: header is absent → serve `200 OK` with the
//!   whole resource.
//! - [`RangeParse::Single(satisfied)`]: header is a single valid range
//!   → serve `206 Partial Content`.
//! - [`RangeParse::Unsatisfiable`]: header parses but the range falls
//!   outside `0..total_len` (or `total_len == 0`) → serve `416 Range Not
//!   Satisfiable`.

/// Outcome of parsing a `Range:` header against a known content length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeParse {
    /// No `Range:` header (or empty). Caller should serve the whole body
    /// with `200 OK`.
    Full,
    /// Header parsed cleanly and resolves to a satisfiable byte range.
    Single(Satisfied),
    /// Header parsed but the range is unsatisfiable against `total_len`.
    /// Caller should reply `416 Range Not Satisfiable` with
    /// `Content-Range: bytes */{total_len}`.
    Unsatisfiable,
}

/// A satisfiable byte range against a known content length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Satisfied {
    /// Inclusive start byte (`0` ≤ start ≤ `end`).
    pub start: u64,
    /// Inclusive end byte (`end` < `total_len`).
    pub end: u64,
    /// Total content length the range was resolved against.
    pub total_len: u64,
}

impl Satisfied {
    /// Number of bytes to send. Always `end - start + 1`.
    #[must_use]
    pub fn content_length(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Value of the `Content-Range` response header.
    #[must_use]
    pub fn content_range_header(&self) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, self.total_len)
    }
}

/// Parse a `Range:` header value against `total_len`. Accepts an
/// `Option<&str>` so callers can pass `headers.get("range")` directly.
///
/// Supported forms (per RFC 7233 §2.1, single-range only):
///
/// - `bytes=N-M` — explicit start and end (inclusive)
/// - `bytes=N-` — start at N, run to EOF
/// - `bytes=-N` — last N bytes (suffix range)
///
/// Anything else returns [`RangeParse::Unsatisfiable`]. A `total_len` of
/// `0` always returns `Unsatisfiable` (no bytes exist to send).
#[must_use]
pub fn parse(header: Option<&str>, total_len: u64) -> RangeParse {
    let Some(raw) = header else {
        return RangeParse::Full;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return RangeParse::Full;
    }

    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangeParse::Unsatisfiable;
    };
    // Single range only — RFC 7233 §2.1 allows comma-separated lists, we
    // refuse them rather than degrade to the first.
    if spec.contains(',') {
        return RangeParse::Unsatisfiable;
    }
    if total_len == 0 {
        return RangeParse::Unsatisfiable;
    }

    let last = total_len - 1;
    let parts: Vec<&str> = spec.splitn(2, '-').collect();
    if parts.len() != 2 {
        return RangeParse::Unsatisfiable;
    }
    let (start_s, end_s) = (parts[0].trim(), parts[1].trim());

    match (start_s.is_empty(), end_s.is_empty()) {
        (true, true) => RangeParse::Unsatisfiable,
        // Suffix range: "bytes=-N" → last N bytes.
        (true, false) => match end_s.parse::<u64>() {
            Ok(n) if n > 0 => {
                let n = n.min(total_len);
                let start = total_len - n;
                RangeParse::Single(Satisfied {
                    start,
                    end: last,
                    total_len,
                })
            }
            _ => RangeParse::Unsatisfiable,
        },
        // Open-ended range: "bytes=N-" → N..EOF
        (false, true) => match start_s.parse::<u64>() {
            Ok(start) if start <= last => RangeParse::Single(Satisfied {
                start,
                end: last,
                total_len,
            }),
            _ => RangeParse::Unsatisfiable,
        },
        // Closed range: "bytes=N-M"
        (false, false) => match (start_s.parse::<u64>(), end_s.parse::<u64>()) {
            (Ok(start), Ok(end)) if start <= end && start <= last => {
                RangeParse::Single(Satisfied {
                    start,
                    end: end.min(last),
                    total_len,
                })
            }
            _ => RangeParse::Unsatisfiable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_header_returns_full() {
        assert_eq!(parse(None, 1024), RangeParse::Full);
        assert_eq!(parse(Some(""), 1024), RangeParse::Full);
        assert_eq!(parse(Some("   "), 1024), RangeParse::Full);
    }

    #[test]
    fn missing_bytes_prefix_is_unsatisfiable() {
        assert_eq!(parse(Some("0-100"), 1024), RangeParse::Unsatisfiable);
        assert_eq!(parse(Some("items=0-100"), 1024), RangeParse::Unsatisfiable);
    }

    #[test]
    fn closed_range_within_bounds() {
        let RangeParse::Single(s) = parse(Some("bytes=0-99"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.start, 0);
        assert_eq!(s.end, 99);
        assert_eq!(s.total_len, 1024);
        assert_eq!(s.content_length(), 100);
        assert_eq!(s.content_range_header(), "bytes 0-99/1024");
    }

    #[test]
    fn closed_range_end_clipped_to_last() {
        let RangeParse::Single(s) = parse(Some("bytes=0-9999"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.end, 1023);
    }

    #[test]
    fn open_ended_range_runs_to_eof() {
        let RangeParse::Single(s) = parse(Some("bytes=500-"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.start, 500);
        assert_eq!(s.end, 1023);
    }

    #[test]
    fn open_ended_range_at_eof_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=1024-"), 1024), RangeParse::Unsatisfiable);
    }

    #[test]
    fn suffix_range_returns_last_n_bytes() {
        let RangeParse::Single(s) = parse(Some("bytes=-100"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.start, 924);
        assert_eq!(s.end, 1023);
        assert_eq!(s.content_length(), 100);
    }

    #[test]
    fn suffix_range_larger_than_file_clamps() {
        let RangeParse::Single(s) = parse(Some("bytes=-9999"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.start, 0);
        assert_eq!(s.end, 1023);
    }

    #[test]
    fn suffix_range_zero_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=-0"), 1024), RangeParse::Unsatisfiable);
    }

    #[test]
    fn empty_file_is_always_unsatisfiable() {
        assert_eq!(parse(Some("bytes=0-0"), 0), RangeParse::Unsatisfiable);
        assert_eq!(parse(Some("bytes=-1"), 0), RangeParse::Unsatisfiable);
        assert_eq!(parse(None, 0), RangeParse::Full);
    }

    #[test]
    fn malformed_input_is_unsatisfiable() {
        assert_eq!(
            parse(Some("bytes=abc-def"), 1024),
            RangeParse::Unsatisfiable
        );
        assert_eq!(parse(Some("bytes=10-5"), 1024), RangeParse::Unsatisfiable);
        assert_eq!(parse(Some("bytes=-"), 1024), RangeParse::Unsatisfiable);
        assert_eq!(parse(Some("bytes="), 1024), RangeParse::Unsatisfiable);
    }

    #[test]
    fn multipart_range_is_refused() {
        assert_eq!(
            parse(Some("bytes=0-99,200-299"), 1024),
            RangeParse::Unsatisfiable
        );
    }

    #[test]
    fn single_byte_range() {
        let RangeParse::Single(s) = parse(Some("bytes=512-512"), 1024) else {
            panic!("expected single range")
        };
        assert_eq!(s.start, 512);
        assert_eq!(s.end, 512);
        assert_eq!(s.content_length(), 1);
    }
}
