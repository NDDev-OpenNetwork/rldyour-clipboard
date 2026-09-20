# Third party notices

## Dependencies

The daemon links the following crates. Each is used under its own licence.

| Crate | Licence | Why it is here |
|---|---|---|
| `rusqlite` (bundled SQLite) | MIT (SQLite itself is public domain) | The archive index and its full-text search. Bundled so the daemon needs nothing from the host. |
| `sha2` | MIT or Apache-2.0 | Content addresses for blobs, and entry identity. |
| `serde`, `serde_json` | MIT or Apache-2.0 | Control frames carry user text; hand-rolled string unescaping would be a correctness risk rather than a saving. |
| `image` | MIT or Apache-2.0 | Thumbnails, and `image/bmp` re-encoding for remote desktops. Optional: `--no-default-features` removes it. |
| `x11rb` | MIT or Apache-2.0 | X11 protocol bindings — the daemon watches `CLIPBOARD` itself on X11/XRDP sessions. Linux only. |
| `objc2`, `objc2-foundation`, `objc2-app-kit` | MIT | `NSPasteboard` access on macOS. |
| `windows-sys` | MIT or Apache-2.0 | Clipboard listener and global memory on Windows. |
| `uds_windows` | MIT | AF_UNIX sockets on Windows, which the standard library does not expose. |

The Python client depends on nothing outside the standard library.

## Precedents

The approach to clipboard capture under GNOME — `MetaSelection`'s
`owner-changed` signal and `transfer_async` — is the one every GNOME clipboard
manager uses, and was established by extensions including Clipboard Indicator
and Pano. No code is taken from them.

The mime hints used to recognise a secret are the ones the ecosystem settled
on rather than a standard: `x-kde-passwordManagerHint` is what KeePassXC,
KWallet and Klipper agree on, and `org.nspasteboard.ConcealedType` is its
macOS equivalent.
