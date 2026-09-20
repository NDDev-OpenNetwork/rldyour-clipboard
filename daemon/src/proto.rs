//! The wire format between the daemon and its clients.
//!
//! One JSON object per line. A frame that declares `bytes` is followed by
//! exactly that many raw bytes, which neither side ever holds whole: the
//! capture client splices them in from the compositor and the daemon splices
//! them out to a blob file. That is what makes an entry's size unbounded in
//! practice and not only on paper.
//!
//! `docs/protocol.md` is the specification; this module is its only encoder
//! and decoder.

use serde::{Deserialize, Serialize};

/// Incremented only on an incompatible change to the frames below.
pub const PROTOCOL_VERSION: u32 = 1;

/// A control line longer than this cannot be a frame this daemon defines, and
/// reading it would mean letting a peer choose how much memory to take.
/// Payloads are not affected: they are streamed and counted, never buffered.
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Feeds clipboard events in; receives no broadcasts.
    Capture,
    /// Browses the archive and asks for entries to be served back.
    #[default]
    Ui,
    /// Does both on one connection.
    Both,
}

impl Role {
    pub fn records(self) -> bool {
        matches!(self, Role::Capture | Role::Both)
    }

    pub fn watches(self) -> bool {
        matches!(self, Role::Ui | Role::Both)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Hello {
        v: u32,
        #[serde(default)]
        role: Role,
    },
    Begin {
        req: u64,
        #[serde(default)]
        source: Option<String>,
    },
    Part {
        req: u64,
        draft: u64,
        mime: String,
        /// Absent when the client does not know the length yet: the payload
        /// then arrives as `chunk` frames ending with a zero-length one.
        #[serde(default)]
        bytes: Option<u64>,
    },
    /// One piece of a length-less part. Carries no `req` because its position
    /// between a part and its terminator is what says where it belongs.
    Chunk {
        bytes: u64,
    },
    Commit {
        req: u64,
        draft: u64,
    },
    Abort {
        req: u64,
        draft: u64,
    },
    List {
        req: u64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        before: Option<i64>,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        kind: Option<String>,
    },
    Fetch {
        req: u64,
        entry: i64,
        #[serde(default)]
        mime: Option<String>,
        /// Asks the daemon to produce the representation by transcoding when
        /// the entry does not literally hold it. The only pair defined is
        /// `image/*` → `image/bmp`, which the RDP clipboard channel relays.
        /// Anything else still answers `no-such-mime`.
        #[serde(default)]
        transcode: bool,
    },
    Thumb {
        req: u64,
        entry: i64,
    },
    Pin {
        req: u64,
        entry: i64,
        pinned: bool,
    },
    Remove {
        req: u64,
        entry: i64,
    },
    Clear {
        req: u64,
    },
    Stats {
        req: u64,
    },
}

impl Request {
    /// The request id to quote in the answer, when the frame carries one.
    ///
    /// `hello` is the one frame without a `req`, because it is answered
    /// positionally and can only ever be the first.
    pub fn req(&self) -> Option<u64> {
        match self {
            Request::Hello { .. } => None,
            Request::Begin { req, .. }
            | Request::Part { req, .. }
            | Request::Commit { req, .. }
            | Request::Abort { req, .. }
            | Request::List { req, .. }
            | Request::Fetch { req, .. }
            | Request::Thumb { req, .. }
            | Request::Pin { req, .. }
            | Request::Remove { req, .. }
            | Request::Clear { req }
            | Request::Stats { req } => Some(*req),
            Request::Chunk { .. } => None,
        }
    }
}

/// What the UI needs to draw one archive entry without fetching its content.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: i64,
    pub kind: &'static str,
    pub mimes: Vec<String>,
    pub bytes: i64,
    pub preview: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumb: bool,
    pub pinned: bool,
    pub source: Option<String>,
    pub at: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Response {
    Hello {
        v: u32,
        entries: i64,
        bytes: i64,
        budget: i64,
    },
    Ok {
        req: u64,
    },
    Begin {
        req: u64,
        draft: u64,
    },
    Commit {
        req: u64,
        entry: i64,
        created: bool,
    },
    List {
        req: u64,
        items: Vec<Summary>,
    },
    /// Always followed by `bytes` raw bytes.
    Blob {
        req: u64,
        mime: String,
        bytes: u64,
    },
    /// A thumbnail, as straight RGBA followed by `bytes` raw bytes.
    ///
    /// Separate from `blob` because it is pixels rather than a file: the only
    /// way a GNOME Shell extension can draw arbitrary image data is to hand
    /// `St.ImageContent` a buffer and its geometry, so the geometry travels
    /// with it rather than having to be decoded out of a container.
    Thumb {
        req: u64,
        width: u32,
        height: u32,
        stride: u32,
        bytes: u64,
    },
    Stats {
        req: u64,
        entries: i64,
        bytes: i64,
        budget: i64,
    },
    Added {
        entry: Summary,
    },
    Updated {
        entry: Summary,
    },
    Removed {
        entry: i64,
    },
    Cleared {},
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        req: Option<u64>,
        code: &'static str,
        message: String,
    },
}

/// Stable failure codes. A client treats an unknown one as fatal for the
/// request it names and for nothing else.
pub mod code {
    pub const BAD_FRAME: &str = "bad-frame";
    pub const BAD_VERSION: &str = "bad-version";
    pub const NO_SUCH_ENTRY: &str = "no-such-entry";
    pub const NO_SUCH_MIME: &str = "no-such-mime";
    pub const NO_SUCH_DRAFT: &str = "no-such-draft";
    pub const TOO_LARGE: &str = "too-large";
    pub const STORAGE: &str = "storage";
}

impl Response {
    pub fn error(req: Option<u64>, code: &'static str, message: impl Into<String>) -> Self {
        Response::Error {
            req,
            code,
            message: message.into(),
        }
    }

    /// Renders the frame and its terminating newline into `out`, replacing any
    /// previous contents. The payload, when there is one, is written by the
    /// caller straight after and never passes through here.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        // The frames are closed types built from owned Rust values, so the
        // only way this fails is an allocation failure, which is not a case
        // the daemon can act on differently from any other.
        if serde_json::to_writer(&mut *out, self).is_err() {
            out.clear();
            out.extend_from_slice(
                br#"{"ev":"error","code":"storage","message":"frame could not be encoded"}"#,
            );
        }
        out.push(b'\n');
    }
}

/// Parses one control line.
pub fn decode(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_request_shape() {
        assert!(matches!(
            decode(r#"{"op":"hello","v":1,"role":"capture"}"#).unwrap(),
            Request::Hello {
                v: 1,
                role: Role::Capture
            }
        ));
        // The role is optional and a browsing client is the sensible default.
        assert!(matches!(
            decode(r#"{"op":"hello","v":1}"#).unwrap(),
            Request::Hello { role: Role::Ui, .. }
        ));
        assert!(matches!(
            decode(r#"{"op":"part","req":2,"draft":7,"mime":"image/png","bytes":184320}"#).unwrap(),
            Request::Part {
                bytes: Some(184320),
                ..
            }
        ));
        // A part that omits its length is the chunked form.
        assert!(matches!(
            decode(r#"{"op":"part","req":2,"draft":7,"mime":"image/png"}"#).unwrap(),
            Request::Part { bytes: None, .. }
        ));
        assert!(matches!(
            decode(r#"{"op":"chunk","bytes":0}"#).unwrap(),
            Request::Chunk { bytes: 0 }
        ));
        assert!(matches!(
            decode(r#"{"op":"clear","req":9}"#).unwrap(),
            Request::Clear { req: 9 }
        ));
    }

    #[test]
    fn a_part_is_the_only_frame_that_declares_a_payload() {
        match decode(r#"{"op":"part","req":1,"draft":1,"mime":"text/plain","bytes":12}"#).unwrap() {
            Request::Part { bytes, mime, .. } => {
                assert_eq!(bytes, Some(12));
                assert_eq!(mime, "text/plain");
            }
            other => panic!("expected a part, got {other:?}"),
        }
    }

    #[test]
    fn rejects_frames_that_are_not_this_protocol() {
        assert!(decode("hello").is_err());
        assert!(decode(r#"{"op":"nonesuch","req":1}"#).is_err());
        // A known op with the wrong field types is as unusable as an unknown one.
        assert!(decode(r#"{"op":"pin","req":1,"entry":"forty-two","pinned":true}"#).is_err());
    }

    #[test]
    fn user_text_survives_a_round_trip_through_a_query() {
        // Escapes and a surrogate pair: the exact reason this is not parsed by
        // hand.
        let request = decode(r#"{"op":"list","req":1,"query":"\"quoted\"\n😀"}"#).unwrap();
        match request {
            Request::List { query, .. } => assert_eq!(query.unwrap(), "\"quoted\"\n😀"),
            _ => panic!("expected a list"),
        }
    }

    #[test]
    fn encodes_a_frame_with_one_trailing_newline() {
        let mut out = Vec::new();
        Response::Ok { req: 3 }.encode(&mut out);
        assert_eq!(out, br#"{"ev":"ok","req":3}"#.to_vec().tap_newline());
    }

    #[test]
    fn an_error_without_a_request_omits_the_field() {
        let mut out = Vec::new();
        Response::error(None, code::BAD_FRAME, "not a frame").encode(&mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("\"req\""), "{text}");
        assert!(text.contains(r#""code":"bad-frame""#), "{text}");
    }

    /// Small helper so the expected value above reads as the frame plus its
    /// terminator rather than a concatenation.
    trait TapNewline {
        fn tap_newline(self) -> Vec<u8>;
    }

    impl TapNewline for Vec<u8> {
        fn tap_newline(mut self) -> Vec<u8> {
            self.push(b'\n');
            self
        }
    }
}
