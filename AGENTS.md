# rldyour-clipboard — working notes

Clipboard archive: a Rust daemon owns the store; thin platform clients capture
and restore. Linux capture is split — the daemon watches X11 itself (XFIXES),
the GNOME Shell extension covers Wayland. macOS and Windows capture natively
inside the daemon.

## Layout

| Path | What it is |
|---|---|
| `daemon/` | Rust daemon: `capture/` (per-OS backends: `x11.rs`, `macos.rs`, `windows.rs`), `store/` (SQLite index + content-addressed blobs + thumbnails), `config.rs`, `server.rs`, `maintenance.rs`, `net.rs`, `proto.rs`, `session.rs`, `kind.rs` (mime ranking / sensitive hints / protocol-target filtering) |
| `daemon/src/storage_fs/` | Common absolute/regular path boundary, private file creation, POSIX no-follow/directory sync and Windows reparse/open/file-flush semantics |
| `extension/` | GNOME Shell extension (GNOME 46–50): `lib/capture.js`, `restore.js`, `client.js`, `mimes.js`, `indicator.js` |
| `macos/` | Native AppKit menu; typed models, bounded streaming socket client, UI; Swift 6, macOS 12+ |
| `python/` | Dependency-free protocol client + CLI |
| `daemon/systemd/` | `rldyour-clipboardd.{service,socket}` — socket-activated, sandboxed |
| `daemon/launchd/` | macOS agent plist (`launch_activate_socket("sock")` contract) |
| `scripts/` | Consistency/version checks and synthetic integration tests; platform installers live at the repository root |

## Verify

```sh
cd daemon && cargo test --all-features && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
./scripts/check-extension.sh && gjs -m extension/tests/smoke.js
cd python && python3 -m pytest
./scripts/check-consistency.sh
test_root=$(mktemp -d /tmp/cb.XXXX)
export RLDYOUR_CLIPBOARD_HOME=$(python3 -c 'from pathlib import Path; import sys; print(Path(sys.argv[1]).resolve())' "$test_root")
RLDYOUR_CLIPBOARD_CAPTURE=0 ./daemon/target/debug/rldyour-clipboardd &
./scripts/e2e-sample.py   # against the spawned daemon's socket
```

## Invariants guarded by check-consistency.sh

Duplicated facts that must not drift: mime rank table and `SENSITIVE` hints
(`kind.rs` ↔ `mimes.js`), `PROTOCOL_TARGETS` and `MAX_REPRESENTATIONS`,
`PROTOCOL_VERSION` (Rust/JS/Python), `SOCKET_NAME` (code ↔ systemd unit ↔
launchd plist), `MAX_FRAME`. If you touch one side, touch the other and rerun
the script.

## Platform truths that shape the code

- **Wayland/GNOME**: only in-shell code may read the selection; the extension
  streams bytes out and does nothing else. `MetaSelectionSourceMemory` serves
  exactly one mime (`g_strcmp0`), so Wayland restore is single-mime by design.
- **X11/XRDP**: any client can watch `CLIPBOARD`; the daemon does it natively
  (`x11rb` + XFIXES, INCR-aware, owner-change aborts the pending read at once,
  lost X connections reconnect on a capped backoff). Restore goes through
  `xclip` so every requested target is answered.
- **xrdp/cliprdr**: remote copies arrive as `UTF8_STRING`/`text/uri-list`/
  `x-special/gnome-copied-files`/`image/bmp`. Images out to RDP clients must
  be `image/bmp` — `fetch` with `transcode:true` produces it on demand from
  any stored `image/*` (never persisted).
- **Secrets**: entries advertising password-manager/transient hints are
  dropped before bytes are stored — checked at capture and again in `commit`.
- **Favorites = `entry.pinned`**: one durable flag is the whole model. Pinned
  rows are skipped by eviction and `clear`, so the favorites list survives
  restarts and reboots with the archive. `list` takes `pinned: bool` to page
  favorites alone; `stats` adds a `pinned` count. Picker tabs: Recent
  (`pinned:false`, last 10 first) vs Favorites (`pinned:true`, PAGE-sized).
- **Paging is keyset, not `id <`**: `before` resolves the cursor row's
  `(pinned, at)` because the order is `pinned DESC, at DESC` — an id bound
  drops rows at the pinned seam and past re-copied (touched) entries. A
  removed cursor row falls back to the id approximation.
- **Lifecycle**: socket-activated daemon stays resident while it can watch the
  clipboard itself; it idle-exits (2 min) only when nothing is capturing.
- `RLDYOUR_CLIPBOARD_HOME` relocates archive **and** socket together on every
  platform; clients resolve env first, then the platform convention.

## Ops

- Live service: `systemctl --user status rldyour-clipboardd.{socket,service}`;
  logs `journalctl --user -u rldyour-clipboardd.service`.
- A self-bound daemon refuses a live listener, symlink or regular file; only
  a dead socket is replaced. An exclusive archive lock prevents two processes
  from sweeping or indexing the same store. Always isolate tests with a short
  temporary `RLDYOUR_CLIPBOARD_HOME` and `RLDYOUR_CLIPBOARD_CAPTURE=0`.
- Retention defaults to 7 days since the latest capture. Startup and 60-second
  maintenance expire only unpinned entries, at most 4 batches of 256 per wake.
  Pinning uses a FULL-synchronous SQLite commit; unpinning restores the age
  policy and never refreshes the timestamp. Leases protect unfinished drafts.
- Pin acknowledgement requires content buffers to flush first; POSIX also
  syncs blob/fanout/archive directory entries. Missing/redirected content cannot
  become a successful pin. Windows flushes a writable file handle and SQLite
  WAL; do not claim a portable directory-fsync or hardware power-loss guarantee.
- Archive root/lock/index/WAL/SHM and blob paths must not cross redirects or
  special files. Incoming publication uses create_new and private modes, never
  follows a redirected archive path. Same-user deliberate replacement remains
  outside this boundary. Canonicalize only synthetic system temp roots in tests.
- Never inspect, dump or export a user's clipboard for tests. Use synthetic
  fixtures, including the macOS `CLIPBOARD_QA` build (which never writes the
  system pasteboard), and restrict live checks to service/aggregate metadata.
- Never restart the desktop session to test; the daemon side is verifiable
  entirely through the socket and journal.
