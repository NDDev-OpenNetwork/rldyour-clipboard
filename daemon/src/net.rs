//! The local socket, under the one name the rest of the daemon uses.
//!
//! Linux and macOS have `std::os::unix::net`; Windows has had AF_UNIX since
//! Windows 10 but exposes it through a crate rather than the standard library.
//! Aliasing them here keeps every other module free of platform branches.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener as Listener, UnixStream as Stream};
#[cfg(windows)]
pub use uds_windows::{UnixListener as Listener, UnixStream as Stream};

pub const SOCKET_NAME: &str = "rldyour-clipboard.sock";

/// The socket location follows each platform's own convention and only that
/// convention: honouring `XDG_RUNTIME_DIR` elsewhere would strand the clients,
/// which look in the platform directory and nowhere else.
///
/// `RLDYOUR_CLIPBOARD_HOME` moves the socket together with the archive on
/// every platform — the socket belongs beside the archive it serves, and an
/// override that moved one but not the other would split a test deployment in
/// two. A service manager's inherited socket still wins: `bind()` returns it
/// before this is ever asked.
pub fn socket_path() -> std::io::Result<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("RLDYOUR_CLIPBOARD_HOME") {
        let directory = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&directory)?;
        return Ok(directory.join(SOCKET_NAME));
    }

    #[cfg(target_os = "linux")]
    {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            std::io::Error::other("XDG_RUNTIME_DIR is unset and no socket was inherited")
        })?;
        Ok(std::path::Path::new(&runtime).join(SOCKET_NAME))
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Everywhere else the socket sits beside the archive it serves.
        let directory = crate::store::default_root()?;
        std::fs::create_dir_all(&directory)?;
        Ok(directory.join(SOCKET_NAME))
    }
}
