//! Rust clipboard archive: thin CLI entry point, bounded sessions, native
//! capture, transactional index and leased content-addressed blobs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod capture;
mod config;
mod kind;
mod maintenance;
mod net;
mod outbox;
mod proto;
mod server;
mod session;
mod store;
#[cfg(test)]
mod testing;
use std::process::ExitCode;

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
                 \x20                           Pinned entries are never evicted.\n\
                 \x20 RLDYOUR_CLIPBOARD_RETENTION_DAYS  Unpinned retention (default 7; 0 disables)\n\
                 \x20 RLDYOUR_CLIPBOARD_CAPTURE  Set to 0 for isolated protocol tests",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Some(argument) => {
            eprintln!("rldyour-clipboardd: unknown argument {argument:?}");
            return ExitCode::FAILURE;
        }
    }

    match server::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rldyour-clipboardd: {error}");
            ExitCode::FAILURE
        }
    }
}
