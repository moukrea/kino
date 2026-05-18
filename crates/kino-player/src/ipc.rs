//! mpv JSON-IPC framing (PRD §F-015 Linux).
//!
//! mpv's `--input-ipc-server=<socket>` speaks newline-delimited JSON:
//!
//! - Commands:   `{"command": ["loadfile", "url"], "request_id": 1}\n`
//! - Responses:  `{"request_id": 1, "error": "success", "data": ...}\n`
//! - Events:     `{"event": "property-change", "name": "time-pos", ...}\n`
//!
//! This module is pure (no I/O) so the parser is the unit-testable
//! seam. The mpv driver in [`crate::mpv`] composes the parser with a
//! `tokio::net::UnixStream`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::PlayerError;

/// A parsed line from the mpv socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// `request_id` and the parsed response payload.
    Response { request_id: u64, response: Response },
    /// An event the player pushed at us.
    Event(Event),
}

/// Parsed response payload. `error` is mpv's success/failure marker —
/// `"success"` is the only non-error value; any other string is treated
/// as a [`PlayerError::Backend`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Response {
    pub error: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// Subset of mpv events we model explicitly. Everything else is folded
/// into [`Event::Other`] so the driver can ignore it without dropping
/// frames.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `property-change` with a named property and a value payload. mpv
    /// uses this for `time-pos`, `duration`, `pause`, `eof-reached`,
    /// `core-idle`, `paused-for-cache`, and `track-list`.
    PropertyChange { name: String, value: Value },
    /// mpv emits this exactly once on a clean `quit`.
    Shutdown,
    /// mpv emits this when the demuxer reaches end-of-file.
    EndFile {
        /// Mpv `reason` field (`eof`, `stop`, `quit`, `error`, …).
        reason: String,
        /// Optional underlying error string when `reason == "error"`.
        error: Option<String>,
    },
    /// Catch-all: events we don't model individually.
    Other { name: String },
}

/// Outbound command shape. mpv accepts a flat positional array of
/// arguments — the driver always speaks this dialect rather than the
/// newer `{"command_id":..., "command": "loadfile", ...}` form, because
/// the array form has been stable across mpv versions for over a
/// decade.
#[derive(Debug, Clone, Serialize)]
pub struct Command {
    pub command: Vec<Value>,
    pub request_id: u64,
}

impl Command {
    /// Build a command from an array of arguments. The first element is
    /// the command name (`"loadfile"`, `"set"`, `"observe_property"`,
    /// …).
    #[must_use]
    pub fn new(request_id: u64, args: Vec<Value>) -> Self {
        Self {
            command: args,
            request_id,
        }
    }

    /// Serialize to a newline-terminated UTF-8 byte vector ready to
    /// shove at the socket.
    ///
    /// # Panics
    ///
    /// Panics if `serde_json` cannot serialize the payload. The
    /// `Command` shape is finite and contains only owned JSON values,
    /// so this is a programmer error if it ever fires.
    #[must_use]
    pub fn to_line(&self) -> Vec<u8> {
        let mut buf = serde_json::to_vec(self).expect("Command serialization is infallible");
        buf.push(b'\n');
        buf
    }
}

/// Parse one trimmed line from the socket into a [`Frame`].
///
/// `line` must NOT contain the trailing newline; the reader is expected
/// to strip it before calling.
///
/// # Errors
///
/// Returns [`PlayerError::Parse`] if the line is not valid JSON or
/// doesn't match any known frame shape.
pub fn parse_frame(line: &str) -> Result<Frame, PlayerError> {
    let v: Value = serde_json::from_str(line).map_err(|e| PlayerError::Parse {
        message: e.to_string(),
        line: truncate(line, 200),
    })?;
    let obj = v.as_object().ok_or_else(|| PlayerError::Parse {
        message: "frame is not a JSON object".to_string(),
        line: truncate(line, 200),
    })?;
    if let Some(rid) = obj.get("request_id").and_then(Value::as_u64) {
        let error = obj
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("undefined")
            .to_string();
        let data = obj.get("data").cloned();
        return Ok(Frame::Response {
            request_id: rid,
            response: Response { error, data },
        });
    }
    if let Some(event_name) = obj.get("event").and_then(Value::as_str) {
        let parsed = match event_name {
            "property-change" => {
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let value = obj.get("data").cloned().unwrap_or(Value::Null);
                Event::PropertyChange { name, value }
            }
            "shutdown" => Event::Shutdown,
            "end-file" => {
                let reason = obj
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let error = obj
                    .get("file_error")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Event::EndFile { reason, error }
            }
            other => Event::Other {
                name: other.to_string(),
            },
        };
        return Ok(Frame::Event(parsed));
    }
    Err(PlayerError::Parse {
        message: "frame has neither `request_id` nor `event` field".to_string(),
        line: truncate(line, 200),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = String::with_capacity(max + 1);
        out.push_str(s.get(..max).unwrap_or(""));
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_to_line_appends_newline_and_is_valid_json() {
        let cmd = Command::new(7, vec![json!("loadfile"), json!("/tmp/x.mkv")]);
        let line = cmd.to_line();
        assert_eq!(*line.last().unwrap(), b'\n');
        let s = std::str::from_utf8(&line[..line.len() - 1]).unwrap();
        let v: Value = serde_json::from_str(s).unwrap();
        assert_eq!(v["command"][0], "loadfile");
        assert_eq!(v["command"][1], "/tmp/x.mkv");
        assert_eq!(v["request_id"], 7);
    }

    #[test]
    fn parse_response_with_success() {
        let line = r#"{"request_id":42,"error":"success","data":{"time-pos":12.34}}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Response {
                request_id,
                response,
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(response.error, "success");
                let data = response.data.unwrap();
                assert!((data["time-pos"].as_f64().unwrap() - 12.34).abs() < 1e-9);
            }
            other @ Frame::Event(_) => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn parse_response_without_data() {
        let line = r#"{"request_id":1,"error":"property unavailable"}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Response {
                request_id,
                response,
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(response.error, "property unavailable");
                assert!(response.data.is_none());
            }
            other @ Frame::Event(_) => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn parse_event_property_change() {
        let line = r#"{"event":"property-change","id":1,"name":"time-pos","data":42.5}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Event(Event::PropertyChange { name, value }) => {
                assert_eq!(name, "time-pos");
                assert!((value.as_f64().unwrap() - 42.5).abs() < 1e-9);
            }
            other => panic!("expected PropertyChange, got {other:?}"),
        }
    }

    #[test]
    fn parse_event_property_change_with_null_data() {
        // mpv emits `data: null` while a property is unavailable
        // (e.g. `duration` before the demuxer opens).
        let line = r#"{"event":"property-change","id":2,"name":"duration"}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Event(Event::PropertyChange { name, value }) => {
                assert_eq!(name, "duration");
                assert!(value.is_null());
            }
            other => panic!("expected PropertyChange, got {other:?}"),
        }
    }

    #[test]
    fn parse_shutdown_event() {
        let line = r#"{"event":"shutdown"}"#;
        let frame = parse_frame(line).unwrap();
        assert!(matches!(frame, Frame::Event(Event::Shutdown)));
    }

    #[test]
    fn parse_end_file_event_with_reason_and_error() {
        let line =
            r#"{"event":"end-file","reason":"error","file_error":"unrecognized file format"}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Event(Event::EndFile { reason, error }) => {
                assert_eq!(reason, "error");
                assert_eq!(error.as_deref(), Some("unrecognized file format"));
            }
            other => panic!("expected EndFile, got {other:?}"),
        }
    }

    #[test]
    fn parse_end_file_event_eof_reason_no_error() {
        let line = r#"{"event":"end-file","reason":"eof"}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Event(Event::EndFile { reason, error }) => {
                assert_eq!(reason, "eof");
                assert!(error.is_none());
            }
            other => panic!("expected EndFile, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_event_folds_to_other() {
        let line = r#"{"event":"client-message","args":["foo"]}"#;
        let frame = parse_frame(line).unwrap();
        match frame {
            Frame::Event(Event::Other { name }) => assert_eq!(name, "client-message"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_non_object_frames() {
        let err = parse_frame("[1,2,3]").unwrap_err();
        assert!(matches!(err, PlayerError::Parse { .. }));
    }

    #[test]
    fn parse_rejects_malformed_json() {
        let err = parse_frame("{not json").unwrap_err();
        match err {
            PlayerError::Parse { message, line } => {
                assert!(!message.is_empty());
                assert!(line.contains("not json"));
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_object_without_id_or_event() {
        let err = parse_frame(r#"{"foo":"bar"}"#).unwrap_err();
        assert!(matches!(err, PlayerError::Parse { .. }));
    }

    #[test]
    fn truncate_keeps_unicode_safe() {
        let s = "abcdefghij";
        assert_eq!(truncate(s, 100), "abcdefghij");
        assert_eq!(truncate(s, 5), "abcde…");
    }
}
