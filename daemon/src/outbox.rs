//! The writing half of a connection, and the list of connections that want to
//! be told when the archive changes.
//!
//! Every write to a socket goes through one mutex held for the whole frame,
//! payload included. Without that a broadcast could land in the middle of a
//! blob a picker was reading, and the client would have no way to tell the two
//! apart: the payload is opaque bytes and the frame is whatever follows them.

use crate::net::Stream;
use crate::proto::Response;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, Weak};

pub struct Outbox {
    stream: Mutex<Stream>,
}

impl Outbox {
    pub fn new(stream: Stream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }

    /// Writes one frame.
    pub fn send(&self, response: &Response) -> io::Result<()> {
        let mut frame = Vec::with_capacity(256);
        response.encode(&mut frame);

        let mut stream = self.stream.lock().map_err(poisoned)?;
        stream.write_all(&frame)?;
        stream.flush()
    }

    /// Writes one frame and the payload it declared, with nothing able to come
    /// between them.
    ///
    /// The payload is copied straight from the blob file to the socket, so
    /// serving a large entry costs a fixed-size buffer however large it is.
    pub fn send_blob(
        &self,
        response: &Response,
        content: &mut impl Read,
        bytes: u64,
    ) -> io::Result<()> {
        let mut frame = Vec::with_capacity(256);
        response.encode(&mut frame);

        let mut stream = self.stream.lock().map_err(poisoned)?;
        stream.write_all(&frame)?;
        let copied = io::copy(&mut content.take(bytes), &mut *stream)?;
        stream.flush()?;

        if copied != bytes {
            // The frame promised a count the payload did not meet, so the
            // client's stream is now misaligned and cannot be recovered.
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("blob ended {} bytes early", bytes - copied),
            ));
        }
        Ok(())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("a connection writer was poisoned by a panic")
}

/// The connections that asked to be told when the archive changes.
///
/// Held weakly: a client that went away is dropped by its own thread, and the
/// registry notices on the next broadcast rather than needing to be told.
#[derive(Default)]
pub struct Watchers {
    inner: Mutex<Vec<Weak<Outbox>>>,
}

impl Watchers {
    pub fn add(&self, out: &Arc<Outbox>) {
        if let Ok(mut watchers) = self.inner.lock() {
            watchers.push(Arc::downgrade(out));
        }
    }

    /// Tells every watching connection what changed.
    ///
    /// A write that fails is a client that has gone or stopped reading; it is
    /// dropped from the list rather than retried, because the archive must not
    /// wait on anybody to keep recording.
    pub fn broadcast(&self, event: &Response) {
        let Ok(mut watchers) = self.inner.lock() else {
            return;
        };
        watchers.retain(|weak| match weak.upgrade() {
            Some(out) => out.send(event).is_ok(),
            None => false,
        });
    }

    /// How many connections are still listening. The tests observe the
    /// registry through this; nothing else needs to.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .map(|watchers| watchers.iter().filter(|w| w.strong_count() > 0).count())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Summary;

    fn pair() -> (Arc<Outbox>, Stream) {
        let (ours, theirs) = Stream::pair().unwrap();
        (Arc::new(Outbox::new(ours)), theirs)
    }

    fn summary(id: i64) -> Summary {
        Summary {
            id,
            kind: "text",
            mimes: vec!["text/plain".into()],
            bytes: 4,
            preview: Some("text".into()),
            width: None,
            height: None,
            thumb: false,
            pinned: false,
            source: None,
            at: 0,
        }
    }

    #[test]
    fn a_frame_is_one_line() {
        let (out, mut peer) = pair();
        out.send(&Response::Ok { req: 1 }).unwrap();

        let mut read = [0u8; 64];
        let n = peer.read(&mut read).unwrap();
        assert_eq!(&read[..n], b"{\"ev\":\"ok\",\"req\":1}\n");
    }

    #[test]
    fn a_blob_follows_its_frame_with_nothing_in_between() {
        let (out, mut peer) = pair();
        let content = b"0123456789";
        out.send_blob(
            &Response::Blob {
                req: 2,
                mime: "text/plain".into(),
                bytes: 10,
            },
            &mut io::Cursor::new(content),
            10,
        )
        .unwrap();

        let mut reader = io::BufReader::new(&mut peer);
        let mut frame = String::new();
        io::BufRead::read_line(&mut reader, &mut frame).unwrap();
        assert!(frame.contains(r#""bytes":10"#), "{frame}");

        let mut payload = vec![0u8; 10];
        reader.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, content);
    }

    #[test]
    fn a_broadcast_reaches_every_watching_connection() {
        let watchers = Watchers::default();
        let (first, mut first_peer) = pair();
        let (second, mut second_peer) = pair();
        watchers.add(&first);
        watchers.add(&second);
        assert_eq!(watchers.count(), 2);

        watchers.broadcast(&Response::Added { entry: summary(7) });

        for peer in [&mut first_peer, &mut second_peer] {
            let mut frame = String::new();
            io::BufRead::read_line(&mut io::BufReader::new(peer), &mut frame).unwrap();
            assert!(frame.contains(r#""ev":"added""#), "{frame}");
            assert!(frame.contains(r#""id":7"#), "{frame}");
        }
    }

    #[test]
    fn a_connection_that_went_away_leaves_the_list_on_its_own() {
        let watchers = Watchers::default();
        let (out, peer) = pair();
        watchers.add(&out);

        // The client's end closing is what a disconnect looks like from here.
        drop(peer);
        drop(out);

        watchers.broadcast(&Response::Cleared {});
        assert_eq!(watchers.count(), 0);
    }
}
