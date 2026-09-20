# rldyour-clipboard

A clipboard that remembers. One Rust daemon keeps everything you copy — text,
images, file references, at any size — and a tray icon beside your other
indicators opens a picker that puts a chosen entry back where you were typing.

Linux has a GNOME Shell extension; macOS and Windows capture natively inside
the daemon. Every platform exposes the same versioned local protocol.

## Why it is two pieces

The split is not tidiness. Under GNOME the only code that can see the clipboard
is code running inside the shell — mutter implements neither
`wlr-data-control` nor `ext-data-control-v1`, and its maintainers have said it
will not, because handing one application another's clipboard is exactly what
a compositor is there to prevent. So capture has to live in an extension.

That extension runs on the compositor's own thread. Anything it does
synchronously, the desktop does not do. Hashing a screenshot, querying an
index, decoding a thumbnail, waiting on a disk — each of those is a visible
stutter in every animation on screen. So the extension is kept to the one
thing only it can do: it reads the selection and streams it out. It stores
nothing, hashes nothing, decodes nothing, and keeps no history.

Everything else is the daemon, which restarts in milliseconds. Under Wayland
the shell cannot be restarted without logging out, so any change to extension
code costs a session; keeping the extension thin and stable, and putting
everything that evolves behind a socket, is what makes the thing maintainable.

## Unlimited, and what that means

Nothing in the design caps an entry's size.

A control frame states how many bytes follow it, and both sides stream those
bytes rather than holding them. When the length is not known in advance — a
compositor hands a selection over as a stream and never says how long it is —
the payload arrives as chunks ending with a zero-length one. The daemon hashes
each representation as it writes it, in one pass, through a 64 KiB buffer. A
half-gigabyte paste costs the same resident memory as a half-kilobyte one.

This is where it differs from the alternatives. Clipboard managers built as
extensions generally pass content through the shell as a byte array, which is
why they cap what they keep — a popular one stops at four megabytes for
images. Here the bytes never enter the shell's heap at all.

The count of entries is unlimited too. What is rationed is disk:

| | |
|---|---|
| Entries | no limit |
| Disk | 5 GiB by default, configurable |
| Over budget | the oldest unpinned entries are evicted |
| Pinned entries | never evicted |
| One entry larger than the whole budget | refused, not made room for |

Identical content is stored once however many entries reference it, and copying
the same thing twice moves one entry's timestamp rather than adding a second.

## What it keeps

An entry holds **every representation its source offered**, not a flattening of
one. Copying from a browser archives the HTML and the plain text; copying a
screenshot archives the PNG. Entries are classified for display as text, link,
colour, image or files, but that is a presentation hint — the mime types are
what decide what can be served back.

**Thumbnails cost the desktop nothing.** The daemon makes one when it archives
a picture, stores it compressed, and serves it back as ready-to-upload pixels.
So opening a list of forty images uploads forty small buffers and decodes
nothing — where decoding them in the shell would be forty stalls of every
animation on screen. It is also the only thing that works: the shell's texture
cache renders raw data from an `St.ImageContent` and from nothing else, and
quietly draws an empty icon for anything it cannot look up in an icon theme.

**Video, honestly.** No clipboard carries video bytes; copying a video file
puts a *reference* on the clipboard — `text/uri-list` or
`x-special/gnome-copied-files`. Those are archived and pasted back faithfully,
and the row shows a file icon. Extracting a preview frame would need ffmpeg and
is not done.

## Secrets are not archived

An entry that advertises `x-kde-passwordManagerHint` (what KeePassXC, KWallet
and Klipper agree on), the NSPasteboard concealed type, or the Windows
clipboard-history exclusion formats is dropped whole — not stored and then
hidden. The check runs in the capture path, so the bytes never leave the
compositor, and again in the daemon, so a backend cannot forget it.

Applications can also be excluded by window class in the extension's
preferences.

## Install on Linux

Requires Rust 1.85 or newer and GNOME Shell 46 (Ubuntu 24.04 LTS) through 50.

```sh
./install.sh
```

This builds the daemon into `~/.local/bin`, installs a socket-activated user
service, and copies the extension into place. Nothing needs root.

The daemon starts on the first connection and exits two minutes after the last
client disconnects, so it consumes nothing while the picker is closed. The
archive is durable and survives it.

Then load the extension. On X11 the shell reloads in place — press Alt+F2, type
`r`, press Enter. Under Wayland there is no way to do that, so log out and back
in. Either way:

```sh
gnome-extensions enable rldyour-clipboard@nddev-opennetwork
```

To remove the program: `./uninstall.sh`. It leaves the archive alone — what you
copied is your data, and deleting it is a separate, deliberate act.

## Using it

Click the clipboard icon in the tray, or press **Super+V**.

| | |
|---|---|
| Type | searches the whole archive, narrowing as you go |
| Up / Down | move through the list |
| Enter | paste the selected entry |
| Shift+Enter | put it on the clipboard without pasting |
| Escape | clear the search, then close |
| Click a row | paste it |
| Star | pin, so the budget never evicts it |

Choosing an entry puts it on the clipboard and types the paste shortcut into
the window that had the keyboard a moment ago — Ctrl+Shift+V where Ctrl+V
would be wrong, which in a terminal it is.

### The one real limitation

An entry is restored with **one** representation, not all of them.

`MetaSelectionSourceMemory` is the only selection source mutter exposes and it
carries a single mime type. Offering several would need a custom
`MetaSelectionSource`, which cannot be written in an extension: GJS refuses to
implement a vfunc that takes a callback, and `read_async` is exactly that.

So the choice is made to fail safe. Images and file references are restored as
themselves. Text is restored as `text/plain` even when the entry also holds
HTML, because plain text pasted into a rich editor is merely unstyled, whereas
HTML offered to a terminal or a search field matches nothing and pastes
nothing at all. The archive keeps both either way.

## Install on macOS and Windows

The daemon builds and captures natively on both, and is verified on each by CI.
Neither has a tray client yet — the picker is Linux-only in the 0.1 series —
so on those platforms the archive is reached through the protocol or the Python
client.

macOS watches `NSPasteboard`'s change count, which is the documented way to
notice a copy; Windows registers a clipboard format listener on a message-only
window, so it costs nothing between copies. Windows rebuilds a `.bmp` from
`CF_DIB` and strips the `CF_HTML` header, so a screenshot and a rich-text copy
arrive as the same mime types they would on Linux.

## Configuration

The extension's preferences cover what most people change: whether to record at
all, whether to paste on select, the largest item to keep, excluded
applications, and which applications paste with Ctrl+Shift+V.

The archive itself belongs to the daemon, and is set in its service file
(`~/.config/systemd/user/rldyour-clipboardd.service`):

| Variable | Default | Meaning |
|---|---|---|
| `RLDYOUR_CLIPBOARD_BUDGET` | `5368709120` | Bytes before the oldest unpinned entries are evicted; `0` means no budget |
| `RLDYOUR_CLIPBOARD_HOME` | the platform data directory | Where the archive lives |

## Where the archive lives

| Platform | Directory |
|---|---|
| Linux | `~/.local/share/rldyour-clipboard` |
| macOS | `~/Library/Application Support/rldyour-clipboard` |
| Windows | `%LOCALAPPDATA%\rldyour-clipboard` |

`index.db` is SQLite and holds metadata and the full-text index; `blobs/`
holds each representation once, named by the digest of its content. Deleting
that directory deletes the archive and nothing else.

## Protocol

Newline-delimited JSON control frames over a per-user socket at mode 0600 — the
archive holds everything you have ever copied, so the socket is the whole
security boundary. A frame declaring `bytes` is followed by exactly that many
raw bytes, never base64-encoded and never buffered whole.

```json
{"op":"list","req":5,"limit":50,"query":"rebase"}
{"ev":"list","req":5,"items":[
  {"id":42,"kind":"image","mimes":["image/png"],"bytes":184320,
   "preview":null,"width":1920,"height":1080,"thumb":true,
   "pinned":false,"source":"firefox","at":1789456123}]}
```

[`docs/protocol.md`](docs/protocol.md) is the full specification every client
implements. Python clients can use `pip install rldyour-clipboard`:

```sh
rldyour-clipboard list
rldyour-clipboard get 42 > screenshot.png
```

## Checks

```sh
./scripts/check-extension.sh                  # what CI runs for the extension
./scripts/check-consistency.sh                # the facts that exist twice
gjs -m extension/tests/smoke.js               # the extension's own logic
cd daemon && cargo test && cargo clippy --all-targets --all-features -- -D warnings
./scripts/e2e-sample.py                       # against a running daemon
```

`check-extension.sh` covers syntax, metadata, the settings keys the code
actually reads, process isolation between the shell and preferences processes,
deprecated modules, style classes without rules — and two rules specific to
this extension: that no synchronous stream call reaches the shell process, and
that the three APIs which changed between GNOME 46 and 50 (`orientation`,
`vertical`, `set_bytes`) are named only inside the compatibility shim. Each of
those three throws rather than degrading, so getting one wrong costs a session
rather than a layout. None of it needs a running shell, which matters because
Wayland gives no way to reload extension code without a new login.

`check-consistency.sh` compares the things that genuinely have to exist twice:
the mime preference table and the password-manager hints, which the extension
uses to decide what to capture and the daemon uses to decide what to serve
back; and the protocol version, socket name and frame limit, which are written
out in Rust, JavaScript, Python and a systemd unit.

The macOS and Windows capture backends cannot be compiled from a Linux
workstation — the bundled SQLite needs a C toolchain for the target — so CI on
real runners is what proves them.

## Licence

AGPL-3.0-or-later.

This places the extension outside what extensions.gnome.org accepts: the portal
requires every extension to be distributable under GPL-2.0-or-later, which
AGPL-3.0 is not compatible with, and separately forbids shipping binaries,
which this project needs. Distribution is from this repository.
