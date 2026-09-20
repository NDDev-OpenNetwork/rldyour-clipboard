//! Clipboard capture on X11.
//!
//! Under X11 any client may read `CLIPBOARD` — nothing about the protocol is
//! privileged — so the daemon watches the selection itself instead of needing
//! code inside a compositor. That is also what makes it work under XRDP:
//! `xrdp-chansrv` takes selection ownership whenever the remote peer copies,
//! which is indistinguishable from a local application doing it, so a copy on
//! the far side of an RDP session lands in the archive like any other.
//!
//! The watch is event driven: XFixes reports each change of selection owner,
//! and the representations are pulled out of the selection only then. Between
//! copies this thread sits in `wait_for_event` and costs nothing.

use super::Recorder;
use crate::kind;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// How long a single conversion is waited on before it is abandoned.
///
/// An owner that never answers a `ConvertSelection` request must not stall the
/// watch: a stuck read would make every later copy invisible to the archive.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(5);

/// The deadline for each `INCR` window rather than the whole transfer.
///
/// A large selection arrives as a stream of appended chunks; the transfer
/// itself may legitimately take far longer than one conversion, so each chunk
/// gets its own budget and a stalled owner — not a big paste — is what ends it.
const INCR_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait before reconnecting when the X connection dies, and the
/// ceiling the wait grows to.
///
/// The server going away is not a reason to stop watching forever: an XRDP
/// session restart replaces the X server under the session, and a watcher
/// that gave up would leave every later remote copy unrecorded. A failed
/// connect costs a syscall, so retrying indefinitely costs a line a minute.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// Watches `CLIPBOARD`, reconnecting when the X connection drops.
pub fn watch(recorder: &Recorder) {
    let mut backoff = RECONNECT_MIN;
    loop {
        match run(recorder) {
            Err(error) => {
                eprintln!(
                    "rldyour-clipboardd: x11 clipboard watch stopped: {error}; \
                     retrying in {}s",
                    backoff.as_secs()
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
            // `run` only returns on error today; the arm keeps that honest if
            // it ever gains a way to stop cleanly.
            Ok(()) => return,
        }
    }
}

fn run(recorder: &Recorder) -> Result<(), Box<dyn std::error::Error>> {
    let (connection, screen_number) = RustConnection::connect(None)?;
    let screen = &connection.setup().roots[screen_number];
    let root = screen.root;

    // XFixes 5.0 is the version every deployed server has; the selection
    // notification exists since 2.0, so asking for five is simply the
    // version check.
    connection.xfixes_query_version(5, 0)?.reply()?;

    let atoms = Atoms::intern(&connection)?;

    // An InputOnly window is enough: the selection machinery only needs a
    // window to own the requested property on.
    let window = connection.generate_id()?;
    connection.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;

    connection.xfixes_select_selection_input(
        window,
        atoms.clipboard,
        SelectionEventMask::SET_SELECTION_OWNER,
    )?;
    connection.flush()?;
    eprintln!("rldyour-clipboardd: watching CLIPBOARD on the X11 server");

    // Read once at startup: whatever is already on the clipboard is a real
    // copy the archive should hold, and deduplication makes it free when the
    // same entry was captured before this daemon started.
    let mut changed = true;
    loop {
        if changed {
            changed = false;
            read_clipboard(&connection, window, &atoms, recorder, &mut changed);
        }
        match connection.wait_for_event()? {
            Event::XfixesSelectionNotify(notify)
                if notify.selection == atoms.clipboard && notify.owner != window =>
            {
                changed = true;
            }
            _ => {}
        }
    }
}

/// Pulls the offered representations of the current `CLIPBOARD` out of its
/// owner and hands them to the archive.
///
/// `changed` is set again if the owner changes while we are still reading:
/// the selection's content belongs to whoever owns it *now*, so the read in
/// flight is dropped and the new owner is read instead.
fn read_clipboard(
    connection: &RustConnection,
    window: u32,
    atoms: &Atoms,
    recorder: &Recorder,
    changed: &mut bool,
) {
    let Some(offered) = read_targets(connection, window, atoms, changed) else {
        return;
    };

    // One secret hint condemns the whole event, whatever else it offers — the
    // bytes are never requested, let alone stored.
    let names: Vec<String> = offered.iter().map(|(_, name)| name.clone()).collect();
    if kind::any_sensitive(names.iter().map(String::as_str)) {
        return;
    }

    let mut parts = Vec::new();
    for mime in kind::recordable(&names) {
        // The name came from the TARGETS list, so the atom is already known;
        // interning it again would only risk a different spelling.
        let Some((atom, _)) = offered.iter().find(|(_, name)| *name == mime) else {
            continue;
        };
        match read_one(connection, window, atoms, *atom, changed) {
            Some(bytes) if !bytes.is_empty() => parts.push((mime, bytes)),
            Some(_) => eprintln!("rldyour-clipboardd: x11: {mime} empty"),
            None => eprintln!("rldyour-clipboardd: x11: {mime} conversion failed"),
        }
        // The owner changed underneath us: whatever was collected may mix two
        // different copies, so the whole read is dropped and the new owner is
        // read from the top instead.
        if *changed {
            return;
        }
    }

    recorder.record(parts, None);
}

/// Requests `TARGETS` and returns each offered atom with its name.
fn read_targets(
    connection: &RustConnection,
    window: u32,
    atoms: &Atoms,
    changed: &mut bool,
) -> Option<Vec<(Atom, String)>> {
    convert(connection, window, atoms, atoms.targets, changed)?;
    let reply = connection
        .get_property(true, window, atoms.data, AtomEnum::ATOM, 0, u32::MAX / 4)
        .ok()?
        .reply()
        .ok()?;
    let offered: Vec<(Atom, String)> = reply
        .value32()?
        .filter_map(|atom| name_of(connection, atom).map(|name| (atom, name)))
        .collect();
    if offered.is_empty() {
        return None;
    }
    Some(offered)
}

/// Requests one representation and returns its bytes, following `INCR` when
/// the owner answers with an incremental transfer.
fn read_one(
    connection: &RustConnection,
    window: u32,
    atoms: &Atoms,
    target: Atom,
    changed: &mut bool,
) -> Option<Vec<u8>> {
    convert(connection, window, atoms, target, changed)?;

    let first = connection
        .get_property(false, window, atoms.data, AtomEnum::ANY, 0, u32::MAX / 4)
        .ok()?
        .reply()
        .ok()?;

    if first.type_ != atoms.incr {
        // The first read already holds the whole answer — the bytes would
        // only cross the wire twice if it were fetched again. What the ICCCM
        // still wants is the property deleted once the requestor has it.
        connection.delete_property(window, atoms.data).ok()?;
        connection.flush().ok()?;
        return Some(first.value);
    }

    // INCR: deleting the property is the signal the owner waits for before
    // appending the first chunk; each appended chunk is then read and deleted
    // until a zero-length append ends the transfer.
    connection.delete_property(window, atoms.data).ok()?;
    connection.flush().ok()?;

    let mut bytes = Vec::new();
    let mut deadline = Instant::now() + INCR_TIMEOUT;
    loop {
        match wait_event(connection, atoms.clipboard, deadline, changed)? {
            Event::PropertyNotify(notify)
                if notify.window == window
                    && notify.atom == atoms.data
                    && notify.state == x11rb::protocol::xproto::Property::NEW_VALUE =>
            {
                let chunk = connection
                    .get_property(true, window, atoms.data, AtomEnum::ANY, 0, u32::MAX / 4)
                    .ok()?
                    .reply()
                    .ok()?;
                if chunk.value.is_empty() {
                    return Some(bytes);
                }
                bytes.extend_from_slice(&chunk.value);
                deadline = Instant::now() + INCR_TIMEOUT;
            }
            _ => {}
        }
    }
}

/// Issues a `ConvertSelection` and waits for the owner's `SelectionNotify`.
///
/// Returns `Some(())` when the owner answered with data, `None` when it
/// refused or timed out.
fn convert(
    connection: &RustConnection,
    window: u32,
    atoms: &Atoms,
    target: Atom,
    changed: &mut bool,
) -> Option<()> {
    connection
        .convert_selection(
            window,
            atoms.clipboard,
            target,
            atoms.data,
            x11rb::CURRENT_TIME,
        )
        .ok()?;
    connection.flush().ok()?;

    let deadline = Instant::now() + CONVERT_TIMEOUT;
    loop {
        match wait_event(connection, atoms.clipboard, deadline, changed)? {
            Event::SelectionNotify(notify)
                if notify.requestor == window && notify.selection == atoms.clipboard =>
            {
                // A refused conversion carries property == NONE.
                return (notify.property != x11rb::NONE).then_some(());
            }
            _ => {}
        }
    }
}

/// The next event before `deadline`, marking `changed` when the clipboard's
/// owner moved on while we were still asking the previous owner for data.
///
/// Once it has, there is nothing left to wait out: the new owner will never
/// answer the conversion the old one was asked for, so the read is abandoned
/// immediately rather than when the deadline runs down.
fn wait_event(
    connection: &RustConnection,
    clipboard: Atom,
    deadline: Instant,
    changed: &mut bool,
) -> Option<Event> {
    loop {
        if *changed {
            return None;
        }
        match connection.poll_for_event() {
            Ok(Some(event)) => {
                if let Event::XfixesSelectionNotify(notify) = &event {
                    if notify.selection == clipboard {
                        *changed = true;
                    }
                }
                return Some(event);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                let _ = connection.flush();
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                eprintln!("rldyour-clipboardd: x11: event poll failed: {error}");
                return None;
            }
        }
    }
}

/// Resolves an atom's name; a dead atom is simply not a representation.
fn name_of(connection: &RustConnection, atom: Atom) -> Option<String> {
    connection
        .get_atom_name(atom)
        .ok()?
        .reply()
        .ok()
        .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
}

/// The atoms this backend needs interned once, before the watch begins.
struct Atoms {
    /// The `CLIPBOARD` selection itself — a convention atom, not predefined.
    clipboard: Atom,
    /// Where each conversion asks the owner to leave its answer.
    data: Atom,
    /// The `TARGETS` target, whose answer lists what the owner can serve.
    targets: Atom,
    /// The type an `INCR` answer carries.
    incr: Atom,
}

impl Atoms {
    const DATA_NAME: &'static str = "_RLDYOUR_CLIPBOARD_DATA";

    fn intern(connection: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let intern = |name: &str| -> Result<Atom, Box<dyn std::error::Error>> {
            Ok(connection
                .intern_atom(false, name.as_bytes())?
                .reply()?
                .atom)
        };
        Ok(Self {
            clipboard: intern("CLIPBOARD")?,
            data: intern(Self::DATA_NAME)?,
            targets: intern("TARGETS")?,
            incr: intern("INCR")?,
        })
    }
}
