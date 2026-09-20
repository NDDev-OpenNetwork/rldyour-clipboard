# Changelog

All notable changes to this project are documented here. The daemon, the
Python client and this file are released together and carry the same version.

## 0.1.0

First release.

### The archive

- A Rust daemon owns a clipboard archive: metadata and a full-text index in
  SQLite, content in a blob store addressed by the digest of each
  representation, so two entries sharing content share one file.
- No limit on the number of entries. A byte budget, 5 GiB by default, evicts
  the oldest unpinned entries when the store outgrows it. Pinned entries are
  never evicted, and a single representation larger than the whole budget is
  refused rather than allowed to empty the archive.
- An entry keeps every representation its source offered, so what is archived
  is what was copied rather than a flattening of it.
- Copying the same thing twice moves one entry's timestamp instead of adding a
  second, whatever order the representations arrived in.

### The protocol

- Newline-delimited JSON control frames over a per-user socket at mode 0600,
  each frame optionally followed by a declared count of raw bytes.
- A part whose length is not yet known is sent as chunks ending with a
  zero-length one. This is what makes entry size unbounded in practice: a
  compositor hands a selection over as a stream and never states its length,
  and neither side ever holds a whole entry.
- `fetch` accepts `transcode` for representations the daemon can produce on
  demand — currently `image/bmp` from any stored image, the one image form an
  RDP client can receive.
- `list` accepts a `pinned` filter, so clients can page the favorites list on
  its own; `stats` reports how many entries are pinned. The `before` cursor
  now resolves the row's position in the pinned-first ordering, so paging no
  longer drops entries at the pinned boundary or past a re-copied row.
- The socket sits beside the archive it serves: `RLDYOUR_CLIPBOARD_HOME`
  relocates both on every platform and in every client.
- Specified in `docs/protocol.md`, with a dependency-free Python client.

### Linux

- The daemon watches `CLIPBOARD` itself wherever an X11 server is reachable,
  subscribing to `XFIXES` owner changes and reading every offered target —
  plain and `INCR` transfers alike. This is what puts remote-desktop copies in
  the archive: under XRDP `xrdp-chansrv` owns the selection like any other X11
  client, so RDP text, `text/uri-list` files and `image/bmp` images are
  recorded with no GNOME component involved. A watching daemon stays resident
  rather than idle-exiting.
- A GNOME Shell extension for GNOME 46 through 50. Mutter implements neither
  `wlr-data-control` nor `ext-data-control-v1` and its maintainers have said it
  will not, so code inside the shell is the only thing that can see the
  selection; the extension is kept to exactly that and stores nothing, hashes
  nothing and decodes no images. Under Wayland it is the capture path; under
  X11 it supplements the daemon's own watcher, and deduplication folds the two.
- A tray icon in the same panel box as the AppIndicator icons, opening a picker
  with search, kind filters, thumbnails and paging. One click opens it — which
  a StatusNotifierItem cannot do, since the AppIndicator extension reserves
  `Activate` for a double click.
- The picker has two pages: Recent shows the live stream of the newest copies;
  Favorites holds starred entries — the durable store for prompts, snippets and
  media that eviction and `clear` never touch, paged and searchable like the
  stream. The Python client reaches it with `Client.favorites()` and
  `rldyour-clipboard list --favorites`.
- Choosing an entry puts it on the clipboard and types the paste shortcut into
  the window that had the keyboard, using Ctrl+Shift+V where Ctrl+V would be
  wrong. On X11 the clipboard is owned through `xclip`, which answers every
  paste target an application can request — the compositor's memory source can
  offer only one mime — and image entries are served as `image/bmp` so an RDP
  client on the other end of `cliprdr` can receive them.
- Socket-activated user service; a daemon with nothing to watch exits two
  minutes after the last client disconnects.

### macOS and Windows

- Native capture inside the daemon: `NSPasteboard` change-count polling on
  macOS, `AddClipboardFormatListener` on a message-only window on Windows.
- Windows rebuilds a `.bmp` from `CF_DIB` and strips the `CF_HTML` header, so a
  screenshot and a rich-text copy arrive as the same mime types they would on
  Linux.
- macOS installs socket-activated like Linux — `launchd` hands the daemon a
  listening socket through the shipped `io.nddev.rldyour-clipboardd.plist`.
  Windows has no per-user socket activation, so the daemon binds its own and
  the Run key starts it at sign-in.
- No tray client for either yet; both expose the same protocol.

### Thumbnails

- Made by the daemon, stored compressed, and served as straight RGBA with its
  geometry. The client decodes nothing, which is what lets a list of forty
  images open without the compositor stalling — and is also the only form the
  shell's texture cache will render from raw data.

### Secrets

- An entry offering `x-kde-passwordManagerHint`, the NSPasteboard concealed
  type, or the Windows clipboard-history exclusion formats is dropped whole.
  Checked in the capture path and again in the daemon, so a secret is neither
  read out of the compositor nor written to disk.
