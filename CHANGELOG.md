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
- Specified in `docs/protocol.md`, with a dependency-free Python client.

### Linux

- A GNOME Shell extension for GNOME 46 through 50. Mutter implements neither
  `wlr-data-control` nor `ext-data-control-v1` and its maintainers have said it
  will not, so code inside the shell is the only thing that can see the
  selection; the extension is kept to exactly that and stores nothing, hashes
  nothing and decodes no images.
- A tray icon in the same panel box as the AppIndicator icons, opening a picker
  with search, kind filters, thumbnails and paging. One click opens it — which
  a StatusNotifierItem cannot do, since the AppIndicator extension reserves
  `Activate` for a double click.
- Choosing an entry puts it on the clipboard and types the paste shortcut into
  the window that had the keyboard, using Ctrl+Shift+V where Ctrl+V would be
  wrong.
- Socket-activated user service that exits when nobody has been connected for
  two minutes.

### macOS and Windows

- Native capture inside the daemon: `NSPasteboard` change-count polling on
  macOS, `AddClipboardFormatListener` on a message-only window on Windows.
- Windows rebuilds a `.bmp` from `CF_DIB` and strips the `CF_HTML` header, so a
  screenshot and a rich-text copy arrive as the same mime types they would on
  Linux.
- No tray client for either yet; both expose the same protocol.

### Secrets

- An entry offering `x-kde-passwordManagerHint`, the NSPasteboard concealed
  type, or the Windows clipboard-history exclusion formats is dropped whole.
  Checked in the capture path and again in the daemon, so a secret is neither
  read out of the compositor nor written to disk.
