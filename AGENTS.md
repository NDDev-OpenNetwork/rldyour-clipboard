# rldyour-clipboard — working notes

Clipboard archive: a Rust daemon owns the store; thin platform clients capture
and restore. Linux capture is split — the daemon watches X11 itself (XFIXES),
the GNOME Shell extension covers Wayland. macOS and Windows capture natively
inside the daemon.

## Layout

| Path | What it is |
|---|---|
| `daemon/` | Rust daemon: `capture/` (per-OS backends: `x11.rs`, `macos.rs`, `windows.rs`), `store/` (SQLite index + content-addressed blobs + thumbnails), `net.rs`, `proto.rs`, `session.rs`, `kind.rs` (mime ranking / sensitive hints / protocol-target filtering) |
| `extension/` | GNOME Shell extension (GNOME 46–50): `lib/capture.js`, `restore.js`, `client.js`, `mimes.js`, `indicator.js` |
| `python/` | Dependency-free protocol client + CLI |
| `daemon/systemd/` | `rldyour-clipboardd.{service,socket}` — socket-activated, sandboxed |
| `daemon/launchd/` | macOS agent plist (`launch_activate_socket("sock")` contract) |
| `scripts/` | `check-consistency.sh`, `check-extension.sh`, `e2e-sample.py`, `install.sh` |

## Verify

```sh
cd daemon && cargo test --all-features && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
./scripts/check-extension.sh && gjs -m extension/tests/smoke.js
cd python && python3 -m pytest
./scripts/check-consistency.sh
RLDYOUR_CLIPBOARD_HOME=$(mktemp -d) ./daemon/target/debug/rldyour-clipboardd &
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
  (`x11rb` + XFIXES, INCR-aware, owner-change aborts mid-read). Restore goes
  through `xclip` so every requested target is answered.
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
- **Lifecycle**: socket-activated daemon stays resident while it can watch the
  clipboard itself; it idle-exits (2 min) only when nothing is capturing.
- `RLDYOUR_CLIPBOARD_HOME` relocates archive **and** socket together on every
  platform; clients resolve env first, then the platform convention.

## Ops

- Live service: `systemctl --user status rldyour-clipboardd.{socket,service}`;
  logs `journalctl --user -u rldyour-clipboardd.service`.
- A self-bound daemon removes an existing socket file before binding — a stray
  test process can steal `/run/user/1000/rldyour-clipboard.sock` from the
  socket unit. Symptom: captures work, reads return an empty/stale archive.
  Fix: kill the stray, `systemctl --user restart rldyour-clipboardd.socket`.
- Never restart the desktop session to test; the daemon side is verifiable
  entirely through the socket and journal.
