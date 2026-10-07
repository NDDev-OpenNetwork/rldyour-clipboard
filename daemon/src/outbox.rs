//! Framed socket writes and bounded asynchronous archive notifications.
//! Producers only enqueue events; a slow watcher cannot hold up capture,
//! retention or another peer. A subscription owns and stops its writer worker.
use crate::net::Stream;
use crate::proto::{MAX_FRAME, Response};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, Weak};

struct Writer {
    stream: Stream,
    frame: Vec<u8>,
}
pub struct Outbox {
    writer: Mutex<Writer>,
    shutdown: Option<Stream>,
}

impl Outbox {
    pub fn new(stream: Stream) -> Self {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let shutdown = stream.try_clone().ok();
        Self {
            writer: Mutex::new(Writer {
                stream,
                frame: Vec::with_capacity(1024),
            }),
            shutdown,
        }
    }
    pub fn close(&self) {
        if let Some(stream) = &self.shutdown {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
    pub fn send(&self, response: &Response) -> io::Result<()> {
        let mut writer = self.writer.lock().map_err(poisoned)?;
        let Writer { stream, frame } = &mut *writer;
        response.encode(frame);
        if frame.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "outgoing control frame exceeds protocol limit",
            ));
        }
        stream.write_all(frame)
    }
    pub fn send_blob(
        &self,
        response: &Response,
        content: &mut impl Read,
        bytes: u64,
    ) -> io::Result<()> {
        let mut writer = self.writer.lock().map_err(poisoned)?;
        let Writer { stream, frame } = &mut *writer;
        response.encode(frame);
        stream.write_all(frame)?;
        let copied = io::copy(&mut content.take(bytes), stream)?;
        if copied != bytes {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "blob ended before its declared length",
            ));
        }
        Ok(())
    }
}
fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("connection writer lock poisoned")
}

struct Subscriber {
    out: Weak<Outbox>,
    queue: SyncSender<Arc<Response>>,
}
type Registry = Mutex<HashMap<u64, Subscriber>>;
#[derive(Default)]
pub struct Watchers {
    inner: Arc<Registry>,
    next: AtomicU64,
}
pub struct Subscription {
    id: u64,
    registry: Weak<Registry>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade()
            && let Ok(mut entries) = registry.lock()
        {
            entries.remove(&self.id);
        }
    }
}
impl Watchers {
    pub fn add(&self, out: &Arc<Outbox>) -> Option<Subscription> {
        let (queue, events) = mpsc::sync_channel::<Arc<Response>>(64);
        let weak = Arc::downgrade(out);
        std::thread::Builder::new()
            .name("notify".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    let Some(out) = weak.upgrade() else { break };
                    if out.send(&event).is_err() {
                        out.close();
                        break;
                    }
                }
            })
            .ok()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.inner.lock().ok()?.insert(
            id,
            Subscriber {
                out: Arc::downgrade(out),
                queue,
            },
        );
        Some(Subscription {
            id,
            registry: Arc::downgrade(&self.inner),
        })
    }
    pub fn broadcast(&self, event: &Response) {
        let Ok(mut entries) = self.inner.lock() else {
            return;
        };
        let event = Arc::new(event.clone());
        entries.retain(|_, subscription| {
            let Some(out) = subscription.out.upgrade() else {
                return false;
            };
            if subscription.queue.try_send(Arc::clone(&event)).is_err() {
                out.close();
                return false;
            }
            true
        });
    }
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .map(|entries| {
                entries
                    .values()
                    .filter(|entry| entry.out.strong_count() > 0)
                    .count()
            })
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
        let _first_subscription = watchers.add(&first);
        let _second_subscription = watchers.add(&second);
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
        let _subscription = watchers.add(&out);

        // The client's end closing is what a disconnect looks like from here.
        drop(peer);
        drop(out);

        watchers.broadcast(&Response::Cleared {});
        assert_eq!(watchers.count(), 0);
    }

    #[test]
    fn a_slow_watcher_does_not_block_the_producer_and_a_subscription_cleans_up() {
        let watchers = Watchers::default();
        let (out, _peer) = pair();
        let subscription = watchers.add(&out);
        let before = std::time::Instant::now();
        for entry in 0..2000 {
            watchers.broadcast(&Response::Removed { entry });
        }
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        drop(subscription);
        assert_eq!(watchers.count(), 0);
    }
}
