# rldyour-clipboard protocol

The daemon owns the clipboard archive. Clients speak a line-oriented control
protocol over a per-user local socket, and any control frame may be followed by
an unbounded binary payload. Nothing in the protocol caps the size of a
clipboard entry: a frame states how many bytes follow, and both sides stream
those bytes rather than holding them.

`v` is the protocol version, currently `1`, and is incremented only on an
incompatible change. New fields may appear inside version 1.

## Transport

| Platform | Socket |
|---|---|
| Linux | `$XDG_RUNTIME_DIR/rldyour-clipboard.sock` (normally `/run/user/<uid>/rldyour-clipboard.sock`) |
| macOS | `~/Library/Application Support/rldyour-clipboard/rldyour-clipboard.sock` |
| Windows | `%LOCALAPPDATA%\rldyour-clipboard\rldyour-clipboard.sock` |

The socket is mode `0600`: only the owning user may connect. The archive holds
everything the user has copied, so the socket is the whole security boundary
and it is never widened. On Linux systemd binds it (`SocketMode=0600`); the
daemon applies the same mode when it binds one itself.

## Framing

A frame is one UTF-8 JSON object terminated by `\n`. No frame contains a raw
newline. When a frame carries `"bytes": N` with `N > 0`, exactly `N` bytes of
payload follow the terminating newline, and the next frame begins immediately
after the last payload byte.

```
{"op":"part","req":4,"draft":7,"mime":"image/png","bytes":184320}\n
<184320 raw bytes>
{"op":"commit","req":5,"draft":7}\n
```

A payload is opaque. It is never base64-encoded, never escaped, and never
buffered whole by either side — the capture client splices it in from the
compositor and the daemon splices it out to a blob file. That is what makes
"unlimited" a property of the design rather than a promise.

Responses to a connection's requests are emitted in the order the requests
arrived, so a client may correlate by `req` or by position. Payloads are
written atomically with their frame, so a payload never interleaves with
another frame.

## Handshake

The first frame a client sends states the protocol version it speaks and the
role it takes:

```json
{"op":"hello","v":1,"role":"ui"}
```

| Role | Meaning |
|---|---|
| `capture` | Feeds new clipboard entries. Receives no broadcasts. |
| `ui` | Browses the archive and asks for entries to be served back. Receives broadcasts. |
| `both` | Does each of the above on one connection. |

The daemon answers with the archive's current shape:

```json
{"ev":"hello","v":1,"entries":1284,"bytes":394857213,"budget":5368709120}
```

A client that sends a `v` the daemon does not implement is answered with an
`error` frame and disconnected. A client that sends anything before `hello` is
disconnected without an answer.

## Recording an entry

One clipboard event becomes one entry, and one entry holds every
representation the source offered. Recording is a transaction so that a
half-transferred entry is never visible:

```json
{"op":"begin","req":1,"source":"firefox"}
{"ev":"begin","req":1,"draft":7}
```

`source` is a free-form hint for the UI — the application the content came
from — and may be omitted.

Each representation is streamed as its own part:

```json
{"op":"part","req":2,"draft":7,"mime":"text/html","bytes":120}
```

Parts may arrive in any order. The daemon hashes each part as it streams it to
disk, so a part costs one pass and no memory beyond the copy buffer.

### Parts of unknown length

A capture client does not always know how large a representation is before it
has read all of it. A compositor hands over a selection as a stream and never
states its length, so a client that had to declare one would have to buffer
the whole thing first — which is exactly the cost this protocol exists to
avoid. Such a part omits `bytes` entirely:

```json
{"op":"part","req":2,"draft":7,"mime":"image/png"}
```

The payload then arrives as a series of chunks, each an ordinary frame with
its own declared length, ending with one of length zero:

```json
{"op":"chunk","bytes":65536}
<65536 raw bytes>
{"op":"chunk","bytes":65536}
<65536 raw bytes>
{"op":"chunk","bytes":1428}
<1428 raw bytes>
{"op":"chunk","bytes":0}
```

The daemon answers the part's `req` once the terminating chunk arrives. A
chunk frame is only meaningful between a length-less part and its terminator,
and carries no `req` of its own because its position says which part it
belongs to. Nothing else may be sent in between: the part is not finished
until its zero chunk arrives.

Both forms cost the same on the daemon's side — one pass, one hash, no
buffering. A client that knows the length should still declare it, because a
single count is cheaper than a frame per chunk.

```json
{"op":"commit","req":3,"draft":7}
{"ev":"commit","req":3,"entry":42,"created":true}
```

On commit the daemon derives the entry's identity from the sorted set of
`(mime, content hash)` pairs. If an identical entry already exists, nothing is
written: the existing entry's timestamp moves to now, `created` is `false`, and
the returned `entry` is the existing id. This is what stops a clipboard that is
re-asserted every few seconds from filling the archive with copies.

`{"op":"abort","req":4,"draft":7}` discards a draft and its parts. A connection
that closes with drafts open aborts them.

## Browsing

```json
{"op":"list","req":5,"limit":50,"before":91,"query":"rebase","kind":"text"}
```

| Field | Meaning |
|---|---|
| `limit` | At most this many entries; the daemon caps it at 500 |
| `before` | Return only entries older than this id, for paging |
| `query` | Full-text match over text representations and file names; omitted means everything |
| `kind` | Restrict to one kind; omitted means every kind |

The answer carries entry summaries, newest first, pinned entries before the
rest:

```json
{"ev":"list","req":5,"items":[
  {"id":42,"kind":"image","mimes":["image/png"],"bytes":184320,
   "preview":null,"width":1920,"height":1080,"thumb":true,
   "pinned":false,"source":"firefox","at":1789456123}
]}
```

| Field | Type | Meaning |
|---|---|---|
| `id` | int | Stable for the entry's whole life |
| `kind` | string | `text`, `link`, `color`, `image` or `files` |
| `mimes` | [string] | Every representation held, in the order the UI should prefer |
| `bytes` | int | Total size of all representations |
| `preview` | string\|null | Short single-line text preview, already truncated |
| `width`/`height` | int\|null | Pixel size, images only |
| `thumb` | bool | Whether a thumbnail can be fetched |
| `pinned` | bool | Pinned entries are never evicted |
| `source` | string\|null | The application hint recorded at capture |
| `at` | int | Unix seconds of the most recent copy |

`kind` is a presentation hint derived at commit, never a second source of
truth: `mimes` is what actually decides what can be served.

## Serving an entry back

```json
{"op":"fetch","req":6,"entry":42,"mime":"image/png"}
{"ev":"blob","req":6,"mime":"image/png","bytes":184320}
<184320 raw bytes>
```

Omitting `mime` serves the entry's preferred representation and names it in the
answer. A thumbnail is fetched the same way and is always a PNG:

```json
{"op":"thumb","req":7,"entry":42}
{"ev":"blob","req":7,"mime":"image/png","bytes":4096}
```

Thumbnails exist only for entries whose summary says `"thumb":true`. Asking for
one that does not exist is an `error`, not an empty payload.

## Managing the archive

```json
{"op":"pin","req":8,"entry":42,"pinned":true}
{"op":"remove","req":9,"entry":42}
{"op":"clear","req":10}
{"op":"stats","req":11}
```

`clear` removes everything that is not pinned. `stats` answers with the same
shape as the `hello` acknowledgement. The others answer `{"ev":"ok","req":N}`.

## Broadcasts

A `ui` client receives these without asking, and they carry no `req`:

```json
{"ev":"added","entry":{ ...summary... }}
{"ev":"updated","entry":{ ...summary... }}
{"ev":"removed","entry":42}
{"ev":"cleared"}
```

`updated` covers a re-copy of existing content, whose timestamp moved, and a
change of pinned state.

## Errors

```json
{"ev":"error","req":6,"code":"no-such-entry","message":"entry 42 is gone"}
```

`req` is absent when the failure had no request to blame. Codes are stable:
`bad-frame`, `bad-version`, `no-such-entry`, `no-such-mime`, `no-such-draft`,
`too-large`, `storage`. A client should treat an unknown code as fatal for that
request only, not for the connection.

## Lifecycle

Linux and macOS use socket activation, so the daemon may exit once no client
has been connected for thirty seconds and is respawned by the next connection.
Unlike a metrics daemon it holds durable state, so it commits every entry
before it goes. Windows starts it at login and keeps it resident. A client that
finds no listener should reconnect on a short backoff rather than treat it as
an error.

## Storage

The archive lives in one directory, which is the only thing that needs backing
up or deleting:

| Platform | Directory |
|---|---|
| Linux | `$XDG_DATA_HOME/rldyour-clipboard` (normally `~/.local/share/rldyour-clipboard`) |
| macOS | `~/Library/Application Support/rldyour-clipboard` |
| Windows | `%LOCALAPPDATA%\rldyour-clipboard` |

`index.db` is SQLite and holds metadata and the full-text index. `blobs/ab/cdef…`
holds each representation once, named by the hex digest of its content, so two
entries sharing a representation share one file.

There is no limit on the number of entries. There is a limit on bytes: when the
blob store exceeds the configured budget, the daemon evicts whole entries,
oldest first, skipping pinned ones, until it is under budget again. A single
representation larger than the budget is refused with `too-large` rather than
evicting the entire archive to hold it.
