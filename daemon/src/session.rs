//! One connection's conversation with the daemon.
//!
//! A session reads frames, performs them against the archive and writes the
//! answers. Two rules shape everything here:
//!
//! * A payload is read through the same buffered reader the frames are, never
//!   from the socket underneath it. The reader has already pulled payload
//!   bytes into its buffer by the time the frame is parsed, so reading them
//!   anywhere else would lose them.
//! * A payload is always consumed, even when the frame that declared it turns
//!   out to be unusable. Skipping that would leave the stream positioned in
//!   the middle of a blob and every later frame would be nonsense.

use crate::kind::Kind;
use crate::outbox::{Outbox, Watchers};
use crate::proto::{self, Request, Response, Role, code};
use crate::store::{Accepted, Store, StoreError};
use std::collections::HashMap;
use std::io::{self, BufRead, Read};
use std::sync::Arc;

/// A recording in progress: the representations accepted so far, published as
/// one entry when the client commits.
struct Draft {
    source: Option<String>,
    parts: Vec<Accepted>,
}

pub struct Session {
    store: Arc<Store>,
    out: Arc<Outbox>,
    watchers: Arc<Watchers>,
    role: Role,
    drafts: HashMap<u64, Draft>,
    next_draft: u64,
}

impl Session {
    pub fn new(store: Arc<Store>, out: Arc<Outbox>, watchers: Arc<Watchers>) -> Self {
        Self {
            store,
            out,
            watchers,
            role: Role::default(),
            drafts: HashMap::new(),
            next_draft: 1,
        }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Reads the opening frame and answers it.
    ///
    /// A client that does not open with a `hello` it can be held to is not one
    /// this daemon talks to, and it is disconnected rather than corrected.
    pub fn greet(&mut self, input: &mut impl BufRead) -> io::Result<bool> {
        let Some(line) = read_frame(input)? else {
            return Ok(false);
        };

        match proto::decode(&line) {
            Ok(Request::Hello { v, role }) if v == proto::PROTOCOL_VERSION => {
                self.role = role;
                let (entries, bytes) = self.store.stats().unwrap_or((0, 0));
                self.out.send(&Response::Hello {
                    v: proto::PROTOCOL_VERSION,
                    entries,
                    bytes,
                    budget: self.store.budget(),
                })?;
                Ok(true)
            }
            Ok(Request::Hello { v, .. }) => {
                self.out.send(&Response::error(
                    None,
                    code::BAD_VERSION,
                    format!(
                        "this daemon speaks version {}, not {v}",
                        proto::PROTOCOL_VERSION
                    ),
                ))?;
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Performs frames until the client goes away.
    pub fn run(&mut self, input: &mut impl BufRead) -> io::Result<()> {
        while let Some(line) = read_frame(input)? {
            let request = match proto::decode(&line) {
                Ok(request) => request,
                Err(reason) => {
                    // No frame means no payload count, so the stream can no
                    // longer be trusted to be at a frame boundary.
                    self.out
                        .send(&Response::error(None, code::BAD_FRAME, reason))?;
                    return Ok(());
                }
            };

            self.perform(request, input)?;
        }
        Ok(())
    }

    fn perform(&mut self, request: Request, input: &mut impl BufRead) -> io::Result<()> {
        let req = request.req().unwrap_or(0);

        match request {
            // A second hello is as meaningless as none: the role is settled.
            Request::Hello { .. } => self.out.send(&Response::error(
                Some(req),
                code::BAD_FRAME,
                "hello may only open a connection",
            )),

            Request::Begin { source, .. } => {
                if !self.role.records() {
                    return self.refuse(req, "this connection did not take the capture role");
                }
                let draft = self.next_draft;
                self.next_draft += 1;
                self.drafts.insert(
                    draft,
                    Draft {
                        source,
                        parts: Vec::new(),
                    },
                );
                self.out.send(&Response::Begin { req, draft })
            }

            Request::Part {
                draft, mime, bytes, ..
            } => self.part(req, draft, mime, bytes, input),

            // A chunk outside a length-less part has no part to belong to, and
            // its payload cannot be placed. The stream is still at a frame
            // boundary, so the connection survives.
            Request::Chunk { bytes } => {
                drain(input, bytes)?;
                self.out.send(&Response::error(
                    None,
                    code::BAD_FRAME,
                    "a chunk may only follow a part that declared no length",
                ))
            }

            Request::Commit { draft, .. } => self.commit(req, draft),

            Request::Abort { draft, .. } => {
                self.drafts.remove(&draft);
                self.out.send(&Response::Ok { req })
            }

            Request::List {
                limit,
                before,
                query,
                kind,
                ..
            } => {
                let only = kind.as_deref().and_then(Kind::parse);
                match self
                    .store
                    .list(limit.unwrap_or(50), before, query.as_deref(), only)
                {
                    Ok(items) => self.out.send(&Response::List { req, items }),
                    Err(error) => self.failed(req, &error),
                }
            }

            Request::Fetch { entry, mime, .. } => {
                match self.store.open_part(entry, mime.as_deref()) {
                    Ok(Some((mime, mut file, bytes))) => self.out.send_blob(
                        &Response::Blob {
                            req,
                            mime: mime.clone(),
                            bytes,
                        },
                        &mut file,
                        bytes,
                    ),
                    Ok(None) => {
                        // Distinguishing the two tells a client whether to ask for
                        // a different representation or to forget the entry.
                        let (code, message) = match self.store.summary(entry) {
                            Ok(Some(_)) => (
                                code::NO_SUCH_MIME,
                                format!("entry {entry} has no such mime"),
                            ),
                            _ => (code::NO_SUCH_ENTRY, format!("entry {entry} is gone")),
                        };
                        self.out.send(&Response::error(Some(req), code, message))
                    }
                    Err(error) => self.failed(req, &error),
                }
            }

            Request::Thumb { entry, .. } => match self.store.thumbnail(entry) {
                Ok(Some(pixels)) => {
                    let bytes = pixels.rgba.len() as u64;
                    self.out.send_blob(
                        &Response::Thumb {
                            req,
                            width: pixels.width,
                            height: pixels.height,
                            stride: pixels.stride(),
                            bytes,
                        },
                        &mut std::io::Cursor::new(&pixels.rgba),
                        bytes,
                    )
                }
                Ok(None) => {
                    // The same distinction `fetch` draws: whether to ask for
                    // something else or to forget the entry.
                    let (code, message) = match self.store.summary(entry) {
                        Ok(Some(_)) => (
                            code::NO_SUCH_MIME,
                            format!("entry {entry} has no thumbnail"),
                        ),
                        _ => (code::NO_SUCH_ENTRY, format!("entry {entry} is gone")),
                    };
                    self.out.send(&Response::error(Some(req), code, message))
                }
                Err(error) => self.failed(req, &error),
            },

            Request::Pin { entry, pinned, .. } => match self.store.set_pinned(entry, pinned) {
                Ok(Some(summary)) => {
                    self.out.send(&Response::Ok { req })?;
                    self.watchers
                        .broadcast(&Response::Updated { entry: summary });
                    Ok(())
                }
                Ok(None) => self.gone(req, entry),
                Err(error) => self.failed(req, &error),
            },

            Request::Remove { entry, .. } => match self.store.remove(entry) {
                Ok(true) => {
                    self.out.send(&Response::Ok { req })?;
                    self.watchers.broadcast(&Response::Removed { entry });
                    Ok(())
                }
                Ok(false) => self.gone(req, entry),
                Err(error) => self.failed(req, &error),
            },

            Request::Clear { .. } => match self.store.clear() {
                Ok(()) => {
                    self.out.send(&Response::Ok { req })?;
                    self.watchers.broadcast(&Response::Cleared {});
                    Ok(())
                }
                Err(error) => self.failed(req, &error),
            },

            Request::Stats { .. } => match self.store.stats() {
                Ok((entries, bytes)) => self.out.send(&Response::Stats {
                    req,
                    entries,
                    bytes,
                    budget: self.store.budget(),
                }),
                Err(error) => self.failed(req, &error),
            },
        }
    }

    /// Streams one representation into the archive.
    ///
    /// However this ends, exactly the declared payload is consumed and no
    /// more. That is the whole difficulty: the frames that follow a part are
    /// on the far side of its bytes, so reading one byte too few or too many
    /// leaves every later frame misaligned, and a payload is opaque so nothing
    /// downstream could notice.
    fn part(
        &mut self,
        req: u64,
        draft: u64,
        mime: String,
        bytes: Option<u64>,
        input: &mut impl BufRead,
    ) -> io::Result<()> {
        let known = self.drafts.contains_key(&draft);

        let outcome = match bytes {
            Some(declared) => self.whole_part(&mime, known, declared, input)?,
            None => self.chunked_part(&mime, known, input)?,
        };

        match outcome {
            PartOutcome::Recorded(accepted) => {
                self.drafts
                    .get_mut(&draft)
                    .expect("a part is only recorded into a draft that is open")
                    .parts
                    .push(accepted);
                self.out.send(&Response::Ok { req })
            }
            PartOutcome::NoDraft => self.no_such_draft(req, draft),
            PartOutcome::Failed(error) => {
                let code = error.code();
                self.out
                    .send(&Response::error(Some(req), code, error.to_string()))
            }
        }
    }

    /// Takes a part that declared its length.
    fn whole_part(
        &self,
        mime: &str,
        known: bool,
        declared: u64,
        input: &mut impl BufRead,
    ) -> io::Result<PartOutcome> {
        if !known {
            drain(input, declared)?;
            return Ok(PartOutcome::NoDraft);
        }

        let mut counted = Counted::new(input);
        let result = self
            .store
            .accept(mime, &mut (&mut counted).take(declared), declared);
        // What the store actually took, which is not `declared` when it
        // refused before reading or failed part way through.
        let consumed = counted.read;

        match result {
            Ok(accepted) => Ok(PartOutcome::Recorded(accepted)),
            Err(error) => {
                drain(input, declared - consumed)?;
                Ok(PartOutcome::Failed(error))
            }
        }
    }

    /// Takes a part that declared no length, reading chunks to the terminator.
    ///
    /// Every chunk is consumed even once the content is known to be unusable,
    /// because the terminator — and everything after it — is on the far side
    /// of them.
    fn chunked_part(
        &self,
        mime: &str,
        known: bool,
        input: &mut impl BufRead,
    ) -> io::Result<PartOutcome> {
        let mut incoming = None;
        let mut failure = None;

        if known {
            match self.store.incoming(mime) {
                Ok(writer) => incoming = Some(writer),
                Err(error) => failure = Some(error),
            }
        }

        loop {
            let Some(frame) = read_frame(input)? else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "a chunked part was cut off before its terminator",
                ));
            };

            let bytes = match proto::decode(&frame) {
                Ok(Request::Chunk { bytes }) => bytes,
                // Anything else abandoned the part mid-way. There is no count
                // to skip, so the stream cannot be brought back to a frame
                // boundary and the connection ends.
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a chunked part was interrupted by another frame",
                    ));
                }
            };

            if bytes == 0 {
                break;
            }

            match incoming.as_mut() {
                Some(writer) => {
                    let mut counted = Counted::new(input);
                    let result = writer.absorb(&mut (&mut counted).take(bytes), bytes);
                    let consumed = counted.read;
                    if let Err(error) = result {
                        drain(input, bytes - consumed)?;
                        failure = Some(error);
                        // Dropping the writer removes its temporary, so a part
                        // that failed leaves nothing half-stored behind.
                        incoming = None;
                    }
                }
                None => drain(input, bytes)?,
            }
        }

        Ok(match (incoming, failure) {
            (_, Some(error)) => PartOutcome::Failed(error),
            (Some(writer), None) => match writer.finish() {
                Ok(accepted) => PartOutcome::Recorded(accepted),
                Err(error) => PartOutcome::Failed(error),
            },
            // No writer and no failure means there was no draft to open one
            // for. The chunks were consumed regardless.
            (None, None) => PartOutcome::NoDraft,
        })
    }

    fn no_such_draft(&self, req: u64, draft: u64) -> io::Result<()> {
        self.out.send(&Response::error(
            Some(req),
            code::NO_SUCH_DRAFT,
            format!("draft {draft} is not open"),
        ))
    }

    fn commit(&mut self, req: u64, draft: u64) -> io::Result<()> {
        let Some(draft) = self.drafts.remove(&draft) else {
            return self.out.send(&Response::error(
                Some(req),
                code::NO_SUCH_DRAFT,
                format!("draft {draft} is not open"),
            ));
        };

        match self
            .store
            .commit(&draft.parts, draft.source.as_deref(), now())
        {
            Ok(Some(done)) => {
                self.out.send(&Response::Commit {
                    req,
                    entry: done.entry,
                    created: done.created,
                })?;
                // Watchers learn what happened to the archive, including what
                // making room for this cost.
                for evicted in done.evicted {
                    self.watchers
                        .broadcast(&Response::Removed { entry: evicted });
                }
                let event = if done.created {
                    Response::Added {
                        entry: done.summary,
                    }
                } else {
                    Response::Updated {
                        entry: done.summary,
                    }
                };
                self.watchers.broadcast(&event);
                Ok(())
            }
            // A draft that was empty or held a secret. The client is told the
            // transaction finished, with no entry to show for it.
            Ok(None) => self.out.send(&Response::Commit {
                req,
                entry: 0,
                created: false,
            }),
            Err(error) => self.failed(req, &error),
        }
    }

    fn refuse(&self, req: u64, message: &str) -> io::Result<()> {
        self.out
            .send(&Response::error(Some(req), code::BAD_FRAME, message))
    }

    fn gone(&self, req: u64, entry: i64) -> io::Result<()> {
        self.out.send(&Response::error(
            Some(req),
            code::NO_SUCH_ENTRY,
            format!("entry {entry} is gone"),
        ))
    }

    fn failed(&self, req: u64, error: &StoreError) -> io::Result<()> {
        self.out
            .send(&Response::error(Some(req), error.code(), error.to_string()))
    }
}

/// How taking one part ended.
enum PartOutcome {
    Recorded(Accepted),
    /// The draft named is not open. The payload was consumed anyway.
    NoDraft,
    Failed(StoreError),
}

/// A reader that remembers how much it handed out.
///
/// Exists for one reason: when a transfer fails part way, the daemon has to
/// skip the rest of the declared payload and not a byte more, and the only
/// thing that knows how much was already taken is whatever was doing the
/// reading.
struct Counted<'a, R: ?Sized> {
    inner: &'a mut R,
    read: u64,
}

impl<'a, R: Read + ?Sized> Counted<'a, R> {
    fn new(inner: &'a mut R) -> Self {
        Self { inner, read: 0 }
    }
}

impl<R: Read + ?Sized> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.read += read as u64;
        Ok(read)
    }
}

/// Reads bytes that cannot be used but must not be left in the stream.
fn drain(input: &mut impl BufRead, bytes: u64) -> io::Result<()> {
    io::copy(&mut input.take(bytes), &mut io::sink())?;
    Ok(())
}

/// Reads one newline-terminated control frame, refusing one too long to be a
/// frame this protocol defines.
///
/// Written out rather than delegated to `read_line`, because that grows its
/// buffer to whatever the peer sends: a peer that never sends a newline would
/// otherwise choose how much memory the daemon takes. Payload bytes are
/// unbounded, but they are counted first and never held.
fn read_frame(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut frame: Vec<u8> = Vec::new();

    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            // End of stream. A partial frame is a truncated conversation, not
            // a frame to act on.
            return Ok(None);
        }

        match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                // Checked here as well as below: a whole over-long frame can
                // arrive in one buffer, newline included, and would otherwise
                // never reach the accumulating branch.
                if frame.len() + end > proto::MAX_FRAME {
                    return Err(too_long());
                }
                frame.extend_from_slice(&available[..end]);
                input.consume(end + 1);
                return match String::from_utf8(frame) {
                    Ok(text) => Ok(Some(text)),
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "control frames are UTF-8",
                    )),
                };
            }
            None => {
                let taken = available.len();
                frame.extend_from_slice(available);
                input.consume(taken);
                if frame.len() > proto::MAX_FRAME {
                    return Err(too_long());
                }
            }
        }
    }
}

fn too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "control frame is longer than any frame this protocol defines",
    )
}

/// Unix seconds. A clock that cannot be read is not a reason to lose a
/// clipboard entry, so the epoch stands in and the entry sorts oldest.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Stream;
    use crate::outbox::Watchers;
    use crate::store::Store;
    use crate::testing::TempDir;
    use std::io::Write;

    fn frames(text: &str) -> io::Result<Vec<String>> {
        let mut input = io::Cursor::new(text.as_bytes().to_vec());
        let mut out = Vec::new();
        while let Some(frame) = read_frame(&mut input)? {
            out.push(frame);
        }
        Ok(out)
    }

    #[test]
    fn reads_frames_one_line_at_a_time() {
        let read = frames("{\"a\":1}\n{\"b\":2}\n").unwrap();
        assert_eq!(read, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn a_frame_without_its_terminator_is_not_a_frame() {
        // A truncated conversation ends; it does not deliver half a frame.
        assert!(frames("{\"a\":1}").unwrap().is_empty());
    }

    #[test]
    fn refuses_a_frame_longer_than_the_protocol_defines() {
        let huge = format!("{}\n", "x".repeat(proto::MAX_FRAME + 1));

        // Arriving whole, newline and all, in a single buffer.
        assert_eq!(
            frames(&huge).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        // And arriving a little at a time, which is what a socket does.
        let mut dribbled = io::BufReader::with_capacity(64, io::Cursor::new(huge.into_bytes()));
        assert_eq!(
            read_frame(&mut dribbled).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_payload_is_read_from_the_same_buffer_the_frames_are() {
        // The reader has already buffered past the frame by the time it is
        // parsed, which is exactly why payloads may not come from elsewhere.
        let mut input = io::BufReader::new(io::Cursor::new(
            b"{\"op\":\"part\"}\nPAYLOAD{\"op\":\"commit\"}\n".to_vec(),
        ));

        assert_eq!(read_frame(&mut input).unwrap().unwrap(), r#"{"op":"part"}"#);

        let mut payload = Vec::new();
        (&mut input).take(7).read_to_end(&mut payload).unwrap();
        assert_eq!(payload, b"PAYLOAD");

        assert_eq!(
            read_frame(&mut input).unwrap().unwrap(),
            r#"{"op":"commit"}"#
        );
    }

    #[test]
    fn draining_leaves_the_stream_at_the_next_frame() {
        let mut input =
            io::BufReader::new(io::Cursor::new(b"0123456789{\"op\":\"commit\"}\n".to_vec()));
        drain(&mut input, 10).unwrap();
        assert_eq!(
            read_frame(&mut input).unwrap().unwrap(),
            r#"{"op":"commit"}"#
        );
    }

    #[test]
    fn control_frames_must_be_text() {
        let mut input = io::Cursor::new(vec![0xff, 0xfe, b'\n']);
        assert!(read_frame(&mut input).is_err());
    }

    /// A live session on one end of a socket pair, driven from the other.
    ///
    /// The desync bugs this guards against are invisible to a unit test of any
    /// single function: they only show up as a *later* frame being misread, so
    /// the test has to speak the whole protocol.
    struct Peer {
        stream: Stream,
        reader: io::BufReader<Stream>,
        _dir: TempDir,
    }

    impl Peer {
        fn open(budget: i64) -> Self {
            let dir = TempDir::new();
            let store = Arc::new(Store::open(dir.path(), budget).unwrap());
            let (ours, theirs) = Stream::pair().unwrap();
            let reading = ours.try_clone().unwrap();
            let out = Arc::new(Outbox::new(ours));

            std::thread::spawn(move || {
                let mut input = io::BufReader::new(reading);
                let mut session = Session::new(store, out, Arc::new(Watchers::default()));
                if session.greet(&mut input).unwrap_or(false) {
                    let _ = session.run(&mut input);
                }
            });

            let reader = io::BufReader::new(theirs.try_clone().unwrap());
            let mut peer = Self {
                stream: theirs,
                reader,
                _dir: dir,
            };
            peer.send(r#"{"op":"hello","v":1,"role":"capture"}"#, b"");
            assert!(peer.answer().contains(r#""ev":"hello""#));
            peer
        }

        fn send(&mut self, frame: &str, payload: &[u8]) {
            self.stream.write_all(frame.as_bytes()).unwrap();
            self.stream.write_all(b"\n").unwrap();
            self.stream.write_all(payload).unwrap();
            self.stream.flush().unwrap();
        }

        fn answer(&mut self) -> String {
            let mut line = String::new();
            BufRead::read_line(&mut self.reader, &mut line).unwrap();
            line
        }
    }

    #[test]
    fn a_refused_part_leaves_the_stream_at_the_next_frame() {
        // A budget of 64 bytes refuses the part below before reading a byte
        // of it, which is the case where nothing has been consumed yet.
        let mut peer = Peer::open(64);
        peer.send(r#"{"op":"begin","req":1}"#, b"");
        assert!(peer.answer().contains(r#""ev":"begin""#));

        let payload = vec![b'x'; 4096];
        peer.send(
            r#"{"op":"part","req":2,"draft":1,"mime":"image/png","bytes":4096}"#,
            &payload,
        );
        let refusal = peer.answer();
        assert!(refusal.contains(r#""code":"too-large""#), "{refusal}");

        // The real assertion: the frame after the refused payload is read as a
        // frame. Over-draining would have eaten it.
        peer.send(r#"{"op":"commit","req":3,"draft":1}"#, b"");
        let commit = peer.answer();
        assert!(commit.contains(r#""req":3"#), "{commit}");
    }

    #[test]
    fn a_part_for_a_draft_that_is_not_open_still_consumes_its_payload() {
        let mut peer = Peer::open(1024 * 1024);
        peer.send(
            r#"{"op":"part","req":1,"draft":99,"mime":"text/plain","bytes":11}"#,
            b"hello world",
        );
        let refusal = peer.answer();
        assert!(refusal.contains(r#""code":"no-such-draft""#), "{refusal}");

        peer.send(r#"{"op":"stats","req":2}"#, b"");
        let stats = peer.answer();
        assert!(stats.contains(r#""ev":"stats""#), "{stats}");
    }

    #[test]
    fn a_chunked_part_for_a_draft_that_is_not_open_is_read_to_its_terminator() {
        let mut peer = Peer::open(1024 * 1024);
        peer.send(
            r#"{"op":"part","req":1,"draft":99,"mime":"text/plain"}"#,
            b"",
        );
        peer.send(r#"{"op":"chunk","bytes":5}"#, b"aaaaa");
        peer.send(r#"{"op":"chunk","bytes":5}"#, b"bbbbb");
        peer.send(r#"{"op":"chunk","bytes":0}"#, b"");

        let refusal = peer.answer();
        assert!(refusal.contains(r#""code":"no-such-draft""#), "{refusal}");

        peer.send(r#"{"op":"stats","req":2}"#, b"");
        assert!(peer.answer().contains(r#""ev":"stats""#));
    }

    #[test]
    fn a_stray_chunk_is_refused_without_losing_the_connection() {
        let mut peer = Peer::open(1024 * 1024);
        peer.send(r#"{"op":"chunk","bytes":4}"#, b"junk");
        let refusal = peer.answer();
        assert!(refusal.contains(r#""code":"bad-frame""#), "{refusal}");

        // Its payload was consumed, so the session carries on.
        peer.send(r#"{"op":"stats","req":1}"#, b"");
        assert!(peer.answer().contains(r#""ev":"stats""#));
    }

    #[test]
    fn a_chunk_that_overruns_the_budget_does_not_eat_the_frames_after_it() {
        // The case that matters: the writer consumes a chunk in full and only
        // then discovers it is over budget. Anything that skipped the chunk a
        // second time would swallow the terminator and everything after it,
        // and a payload is opaque so nothing downstream could notice.
        let mut peer = Peer::open(1024);
        peer.send(r#"{"op":"begin","req":1}"#, b"");
        assert!(peer.answer().contains(r#""ev":"begin""#));

        peer.send(r#"{"op":"part","req":2,"draft":1,"mime":"image/png"}"#, b"");
        let chunk = vec![b'z'; 512];
        for _ in 0..3 {
            peer.send(r#"{"op":"chunk","bytes":512}"#, &chunk);
        }
        peer.send(r#"{"op":"chunk","bytes":0}"#, b"");

        let refusal = peer.answer();
        assert!(refusal.contains(r#""code":"too-large""#), "{refusal}");

        peer.send(r#"{"op":"commit","req":3,"draft":1}"#, b"");
        let commit = peer.answer();
        assert!(commit.contains(r#""req":3"#), "{commit}");
    }

    #[test]
    fn a_missing_thumbnail_and_a_missing_entry_answer_differently() {
        let mut peer = Peer::open(1024 * 1024);
        peer.send(r#"{"op":"begin","req":1}"#, b"");
        peer.answer();
        peer.send(
            r#"{"op":"part","req":2,"draft":1,"mime":"text/plain","bytes":4}"#,
            b"text",
        );
        peer.answer();
        peer.send(r#"{"op":"commit","req":3,"draft":1}"#, b"");
        assert!(peer.answer().contains(r#""entry":1"#));

        // Text has no thumbnail, but the entry is there: the client should
        // ask for something else, not forget the entry.
        peer.send(r#"{"op":"thumb","req":4,"entry":1}"#, b"");
        let no_thumb = peer.answer();
        assert!(no_thumb.contains(r#""code":"no-such-mime""#), "{no_thumb}");

        peer.send(r#"{"op":"thumb","req":5,"entry":999}"#, b"");
        let no_entry = peer.answer();
        assert!(no_entry.contains(r#""code":"no-such-entry""#), "{no_entry}");
    }

    #[test]
    fn a_whole_transaction_records_and_serves_an_entry_back() {
        let mut peer = Peer::open(1024 * 1024);
        peer.send(r#"{"op":"begin","req":1,"source":"test"}"#, b"");
        assert!(peer.answer().contains(r#""draft":1"#));

        peer.send(
            r#"{"op":"part","req":2,"draft":1,"mime":"text/plain","bytes":5}"#,
            b"hello",
        );
        assert!(peer.answer().contains(r#""ev":"ok""#));

        // And a chunked part alongside it, in the same draft.
        peer.send(r#"{"op":"part","req":3,"draft":1,"mime":"text/html"}"#, b"");
        peer.send(r#"{"op":"chunk","bytes":8}"#, b"<b>hello");
        peer.send(r#"{"op":"chunk","bytes":4}"#, b"</b>");
        peer.send(r#"{"op":"chunk","bytes":0}"#, b"");
        assert!(peer.answer().contains(r#""ev":"ok""#));

        peer.send(r#"{"op":"commit","req":4,"draft":1}"#, b"");
        let commit = peer.answer();
        assert!(commit.contains(r#""created":true"#), "{commit}");

        peer.send(
            r#"{"op":"fetch","req":5,"entry":1,"mime":"text/html"}"#,
            b"",
        );
        let blob = peer.answer();
        assert!(blob.contains(r#""bytes":12"#), "{blob}");
        let mut payload = vec![0u8; 12];
        io::Read::read_exact(&mut peer.reader, &mut payload).unwrap();
        assert_eq!(payload, b"<b>hello</b>");
    }
}
