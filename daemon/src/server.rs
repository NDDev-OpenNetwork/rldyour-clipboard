//! Bounded session admission; each transfer remains isolated from capture.
use crate::net::Stream;
use crate::outbox::{Outbox, Watchers};
use crate::session::Session;
use crate::{capture, store};
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const SESSION_STACK: usize = 256 * 1024;
const MAX_CONNECTIONS: usize = 32;
pub(crate) static CONNECTED: AtomicUsize = AtomicUsize::new(0);

pub fn run() -> std::io::Result<()> {
    let config = crate::config::Config::load()?;
    let root = &config.root;
    let (listener, socket_activated) = crate::net::bind()?;
    let store = Arc::new(
        store::Store::open_with_policy(root, config.budget, config.retention_seconds)
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    );

    // A crash between a commit and its blob sweep leaves files nothing points
    // at. Startup is the only moment nothing else is writing, so it is the
    // only safe moment to reclaim them.
    match store.reconcile(root) {
        Ok(0) => {}
        Ok(reclaimed) => eprintln!("rldyour-clipboardd: reclaimed {reclaimed} orphaned bytes"),
        Err(error) => eprintln!("rldyour-clipboardd: could not reconcile the archive: {error}"),
    }

    let watchers = Arc::new(Watchers::default());

    // Where the daemon can see the clipboard itself, it does: natively on
    // macOS and Windows, and on Linux whenever an X11 server is reachable —
    // which is every XRDP session too, and every Wayland session running
    // XWayland, whose bridged selection mirrors Wayland copies (a filtered
    // view of them; the extension still sees the full set). A Wayland
    // session without XWayland is the one place nothing outside the shell
    // may read the selection, so there the extension alone feeds the daemon.
    let capturing = config.capture && capture::spawn(Arc::clone(&store), Arc::clone(&watchers));
    if capturing {
        eprintln!("rldyour-clipboardd: watching the clipboard natively");
    }

    crate::maintenance::expire(&store, &watchers, config.retention_seconds);
    crate::maintenance::spawn(
        Arc::clone(&store),
        Arc::clone(&watchers),
        config.retention_seconds,
        socket_activated && !capturing,
    );

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            // One refused connection is not a reason to stop serving the rest.
            Err(error) => {
                eprintln!("rldyour-clipboardd: could not accept a connection: {error}");
                continue;
            }
        };

        let store = Arc::clone(&store);
        let watchers = Arc::clone(&watchers);
        // Admission is performed by this one accept loop; other threads
        // only decrement the count, so check + increment cannot overshoot.
        if CONNECTED.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            continue;
        }
        CONNECTED.fetch_add(1, Ordering::SeqCst);

        let spawned = std::thread::Builder::new()
            .name("session".into())
            .stack_size(SESSION_STACK)
            .spawn(move || {
                serve(stream, store, watchers);
                CONNECTED.fetch_sub(1, Ordering::SeqCst);
            });

        if spawned.is_err() {
            CONNECTED.fetch_sub(1, Ordering::SeqCst);
            eprintln!("rldyour-clipboardd: could not start a session thread");
        }
    }

    Ok(())
}

fn serve(stream: Stream, store: Arc<store::Store>, watchers: Arc<Watchers>) {
    // The reader and the writer are separate handles on one socket so that a
    // broadcast can be written while this thread is blocked reading.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let out = Arc::new(Outbox::new(stream));
    let mut input = BufReader::new(reading);
    let mut session = Session::new(Arc::clone(&store), Arc::clone(&out), Arc::clone(&watchers));

    match session.greet(&mut input) {
        Ok(true) => {}
        // A client that did not open with a hello this daemon can honour is
        // simply dropped; it has already been told why when there was
        // anything to tell.
        _ => return,
    }

    let _ = input.get_ref().set_read_timeout(None);
    let _subscription = session.watches().then(|| watchers.add(&out));

    if let Err(error) = session.run(&mut input) {
        // A peer that closed mid-conversation is the ordinary way a session
        // ends, not a fault worth reporting.
        if !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
        ) {
            eprintln!("rldyour-clipboardd: session ended: {error}");
        }
    }
}
