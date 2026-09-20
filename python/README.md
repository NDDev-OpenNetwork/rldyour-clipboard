# rldyour-clipboard

Python client for the [rldyour-clipboard](https://github.com/NDDev-OpenNetwork/rldyour-clipboard)
archive daemon: a clipboard history that keeps text, images and file
references at any size, on Linux, macOS and Windows.

The daemon is a separate program. This package speaks its local protocol and
depends on nothing outside the standard library.

```sh
pip install rldyour-clipboard
rldyour-clipboard list
rldyour-clipboard get 42 > screenshot.png
```

```python
from rldyour_clipboard import Client

with Client() as clipboard:
    for entry in clipboard.list(query="rebase"):
        print(entry["id"], entry["preview"])

    mime, content = clipboard.fetch(42)
```

Recording an entry takes every representation the source offered, so a
pasting application still negotiates the one it wants:

```python
with Client(role="capture") as clipboard:
    entry, created = clipboard.record(
        [("text/html", b"<b>hello</b>"), ("text/plain", b"hello")],
        source="my-app",
    )
```

Payloads are streamed by the protocol and are never base64-encoded, so an
entry is limited only by the archive's disk budget.

The protocol is specified in
[`docs/protocol.md`](https://github.com/NDDev-OpenNetwork/rldyour-clipboard/blob/main/docs/protocol.md).

Licence: AGPL-3.0-or-later.
