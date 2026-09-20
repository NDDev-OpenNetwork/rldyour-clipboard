//! rldyour-clipboardd — keeps everything the user has copied, and serves it
//! back on request.
//!
//! The daemon owns the archive and nothing else owns any part of it. A
//! platform client — the GNOME Shell extension on Linux, the menu bar app on
//! macOS, the tray client on Windows — captures clipboard events and streams
//! them here, then asks for entries back when the user picks one. That split
//! is not decoration: the Linux capture runs inside the compositor's own
//! process, where hashing a screenshot or querying an index would stall the
//! whole desktop.
//!
//! One thread per connection, and no async runtime. The archive never has more
//! than a handful of clients — one capture, one picker — and they are
//! long-lived, so a thread each is both the simplest and the cheapest thing
//! that can serve an unbounded stream without blocking the others.

// Release builds are background daemons: no console window should ever appear
// when Windows launches the binary from the Run key.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod capture;
mod kind;
mod net;
mod outbox;
mod proto;
mod session;
mod store;
#[cfg(test)]
mod testing;

use net::{Listener, Stream};
use outbox::{Outbox, Watchers};
use session::Session;
use std::io::BufReader;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// When a service manager owns the listening socket the daemon may exit once
/// nobody is watching; the next connection starts it again. Longer than the
/// metrics daemon's equivalent because starting this one opens a database.
const IDLE_EXIT: Duration = Duration::from_secs(120);
/// How often the idle watch wakes to see whether that grace has run out.
const IDLE_TICK: Duration = Duration::from_secs(10);
/// A session thread parses short frames and copies bytes between a socket and
/// a file. It never recurses and never holds a large frame.
const SESSION_STACK: usize = 256 * 1024;

/// Live connections. The idle watch reads it; every session thread moves it.
static CONNECTED: AtomicUsize = AtomicUsize::new(0);

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version" | "-V") => {
            println!("rldyour-clipboardd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!(
                "rldyour-clipboardd {} — clipboard archive daemon\n\
                 \n\
                 Serves the clipboard archive to clients on its local socket.\n\
                 No arguments are needed; the socket and archive locations and\n\
                 the size budget come from the environment.\n\
                 \n\
                 Environment:\n\
                 \x20 RLDYOUR_CLIPBOARD_HOME    Archive directory (default: the\n\
                 \x20                           platform's own data directory)\n\
                 \x20 RLDYOUR_CLIPBOARD_BUDGET  Bytes the archive may occupy before\n\
                 \x20                           the oldest unpinned entries are\n\
                 \x20                           evicted (default 5368709120, 5 GiB).\n\
                 \x20                           Pinned entries are never evicted.",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Some(argument) => {
            eprintln!("rldyour-clipboardd: unknown argument {argument:?}");
            return ExitCode::FAILURE;
        }
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rldyour-clipboardd: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> std::io::Result<()> {
    let root = archive_root()?;
    let store = Arc::new(
        store::Store::open(&root, budget())
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    );

    // A crash between a commit and its blob sweep leaves files nothing points
    // at. Startup is the only moment nothing else is writing, so it is the
    // only safe moment to reclaim them.
    match store.reconcile(&root) {
        Ok(0) => {}
        Ok(reclaimed) => eprintln!("rldyour-clipboardd: reclaimed {reclaimed} orphaned bytes"),
        Err(error) => eprintln!("rldyour-clipboardd: could not reconcile the archive: {error}"),
    }

    let (listener, socket_activated) = bind()?;
    let watchers = Arc::new(Watchers::default());

    // Where the daemon can see the clipboard itself, it does: natively on
    // macOS and Windows, and on Linux whenever an X11 server is reachable —
    // which is every XRDP session too, and every Wayland session running
    // XWayland, whose bridged selection mirrors Wayland copies (a filtered
    // view of them; the extension still sees the full set). A Wayland
    // session without XWayland is the one place nothing outside the shell
    // may read the selection, so there the extension alone feeds the daemon.
    let capturing = capture::spawn(Arc::clone(&store), Arc::clone(&watchers));
    if capturing {
        eprintln!("rldyour-clipboardd: watching the clipboard natively");
    }

    // The idle exit frees a daemon nobody uses; one that is itself watching
    // the clipboard is in continuous use, so it stays resident.
    if socket_activated && !capturing {
        // Only a manager-owned socket may be left unattended: it is what
        // starts the daemon again on the next connection.
        idle_watch();
    }

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

    if session.role().watches() {
        watchers.add(&out);
    }

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

/// Exits once the grace period has passed with nobody connected.
///
/// The archive is durable and every entry is committed before its request is
/// answered, so there is nothing to flush: the process can simply go, and the
/// next connection brings it back.
fn idle_watch() {
    let spawned = std::thread::Builder::new()
        .name("idle".into())
        .stack_size(32 * 1024)
        .spawn(|| {
            let mut idle_for = Duration::ZERO;
            loop {
                std::thread::sleep(IDLE_TICK);
                if CONNECTED.load(Ordering::SeqCst) > 0 {
                    idle_for = Duration::ZERO;
                    continue;
                }
                idle_for += IDLE_TICK;
                if idle_for >= IDLE_EXIT {
                    std::process::exit(0);
                }
            }
        });

    if spawned.is_err() {
        // Staying resident is a worse outcome than exiting, not a broken one.
        eprintln!("rldyour-clipboardd: no idle watch; the daemon will stay resident");
    }
}

/// Uses the socket the service manager passed in, or binds one under the
/// platform's per-user directory.
///
/// Returns the listener and whether the manager owns it, which decides if the
/// daemon may exit when idle.
fn bind() -> std::io::Result<(Listener, bool)> {
    if let Some(listener) = inherited_listener() {
        return Ok((listener, true));
    }

    let path = net::socket_path()?;

    // A socket file left by an unclean exit would make the bind fail with
    // EADDRINUSE even though nothing is listening.
    let _ = std::fs::remove_file(&path);
    let listener = Listener::bind(&path)?;

    // The archive holds everything the user has ever copied, so the socket is
    // the whole security boundary. A self-bound one gets the same restriction
    // systemd applies rather than whatever the umask happens to be.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok((listener, false))
}

#[cfg(target_os = "linux")]
fn inherited_listener() -> Option<Listener> {
    let listening = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let owner = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    if !(listening.is_some_and(|count| count >= 1) && owner == Some(std::process::id())) {
        return None;
    }
    use std::os::fd::FromRawFd;
    // SAFETY: systemd guarantees descriptor 3 is the listening socket it
    // created for this unit, and it is passed to exactly one process.
    Some(unsafe { Listener::from_raw_fd(3) })
}

#[cfg(target_os = "macos")]
fn inherited_listener() -> Option<Listener> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        /// In libSystem since launchd exists; returns an error code when this
        /// process was not launched with a `Sockets` dictionary.
        fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            cnt: *mut usize,
        ) -> c_int;
    }

    // The key must match the plist's `Sockets` entry.
    let name = CString::new("sock").ok()?;
    let mut fds: *mut c_int = std::ptr::null_mut();
    let mut count: usize = 0;
    if unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) } != 0
        || fds.is_null()
        || count == 0
    {
        return None;
    }
    // The plist declares exactly one socket; the array is launchd-owned and
    // the caller frees it.
    let fd = unsafe { *fds };
    unsafe { libc_free(fds.cast()) };
    // SAFETY: launch_activate_socket returned this descriptor to us; it is a
    // listening socket nobody else owns.
    Some(unsafe { Listener::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    #[link_name = "free"]
    fn libc_free(pointer: *mut std::ffi::c_void);
}

#[cfg(windows)]
fn inherited_listener() -> Option<Listener> {
    // Windows has no per-user service manager with socket activation; the
    // daemon always binds its own and stays resident.
    None
}

fn archive_root() -> std::io::Result<std::path::PathBuf> {
    match std::env::var_os("RLDYOUR_CLIPBOARD_HOME") {
        Some(path) => Ok(std::path::PathBuf::from(path)),
        None => store::default_root(),
    }
}

/// Bytes the archive may occupy before eviction starts.
///
/// There is deliberately no limit on the number of entries: a long history of
/// short text costs almost nothing, and a count is the wrong thing to ration.
/// Zero means no budget at all — the archive grows until the disk says
/// otherwise, which is the user's business and not the daemon's.
fn budget() -> i64 {
    std::env::var("RLDYOUR_CLIPBOARD_BUDGET")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|bytes| *bytes >= 0)
        .map(|bytes| if bytes == 0 { i64::MAX } else { bytes })
        .unwrap_or(store::DEFAULT_BUDGET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_budget_is_the_default_and_zero_means_no_limit() {
        // Read through the same parsing the daemon uses, without touching the
        // process environment other tests share.
        fn parse(value: Option<&str>) -> i64 {
            value
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|bytes| *bytes >= 0)
                .map(|bytes| if bytes == 0 { i64::MAX } else { bytes })
                .unwrap_or(store::DEFAULT_BUDGET)
        }

        assert_eq!(parse(None), store::DEFAULT_BUDGET);
        assert_eq!(parse(Some("1048576")), 1024 * 1024);
        assert_eq!(
            parse(Some("0")),
            i64::MAX,
            "zero is no budget, not no space"
        );
        // Nonsense leaves the default rather than shrinking the archive to it.
        assert_eq!(parse(Some("-1")), store::DEFAULT_BUDGET);
        assert_eq!(parse(Some("plenty")), store::DEFAULT_BUDGET);
    }
}
