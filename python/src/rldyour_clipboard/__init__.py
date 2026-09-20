"""Client for the rldyour-clipboard local protocol.

The daemon speaks newline-delimited JSON control frames, and a frame that
declares ``bytes`` is followed by exactly that many raw bytes. This client
keeps that shape visible rather than hiding it: payloads are returned as
``bytes`` and never re-encoded, so an entry of any size costs one read.

``docs/protocol.md`` in the repository is the specification.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
from pathlib import Path
from typing import Any, Iterator, NamedTuple, Optional, Sequence, TypedDict

__version__ = "0.1.0"

PROTOCOL_VERSION = 1

#: Control frames are small by construction; anything longer is not one.
MAX_FRAME = 64 * 1024


class Summary(TypedDict):
    """One archive entry as the daemon describes it."""

    id: int
    kind: str
    mimes: list[str]
    bytes: int
    preview: Optional[str]
    width: Optional[int]
    height: Optional[int]
    thumb: bool
    pinned: bool
    source: Optional[str]
    at: int


class Thumbnail(NamedTuple):
    """An entry's thumbnail, as straight eight-bit RGBA."""

    width: int
    height: int
    stride: int
    pixels: bytes


class TruncatedPart(RuntimeError):
    """A representation that was only partly sent.

    Distinct from any other failure because of what it means for the draft:
    the daemon holds a short copy of this representation, so the entry must be
    abandoned rather than committed.
    """

    def __init__(self, mime: str) -> None:
        super().__init__(f"the {mime} representation was cut off")
        self.mime = mime


class ProtocolError(RuntimeError):
    """The daemon answered something this client cannot act on."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code


def socket_path() -> Path:
    """Return the daemon socket used by the current platform.

    ``RLDYOUR_CLIPBOARD_HOME`` relocates archive and socket together on every
    platform — the daemon resolves the same way — so the override wins first.
    """
    override = os.environ.get("RLDYOUR_CLIPBOARD_HOME")
    if override:
        return Path(override) / "rldyour-clipboard.sock"
    if sys.platform == "darwin":
        return (
            Path.home()
            / "Library/Application Support/rldyour-clipboard/rldyour-clipboard.sock"
        )
    if os.name == "nt":
        root = os.environ.get("LOCALAPPDATA")
        if not root:
            raise RuntimeError("LOCALAPPDATA is unset")
        return Path(root) / "rldyour-clipboard/rldyour-clipboard.sock"
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return Path(runtime) / "rldyour-clipboard.sock"


class Client:
    """A connection to the local daemon.

    Used as a context manager so the socket is closed even when a request
    raises, which matters because the daemon aborts any open draft when a
    connection drops.
    """

    def __init__(self, path: Optional[Path] = None, role: str = "ui") -> None:
        self._path = path or socket_path()
        self._role = role
        self._socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._socket.connect(str(self._path))
        self._reader = self._socket.makefile("rb")
        self._next_req = 1
        self.hello = self._greet()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        try:
            self._reader.close()
        finally:
            self._socket.close()

    # -- framing ---------------------------------------------------------

    def _send(self, frame: dict[str, Any], payload: bytes = b"") -> None:
        line = json.dumps(frame, separators=(",", ":")).encode() + b"\n"
        # One send for the frame and its payload: the daemon reads them as a
        # unit and anything in between would misalign the stream.
        self._socket.sendall(line + payload)

    def _read_frame(self) -> dict[str, Any]:
        line = self._reader.readline(MAX_FRAME + 1)
        if not line:
            raise ProtocolError("closed", "the daemon closed the connection")
        if not line.endswith(b"\n"):
            raise ProtocolError("bad-frame", "control frame was too long")
        return json.loads(line)

    def _read_payload(self, count: int) -> bytes:
        payload = self._reader.read(count)
        if payload is None or len(payload) != count:
            raise ProtocolError("bad-frame", "payload ended early")
        return payload

    def _greet(self) -> dict[str, Any]:
        self._send({"op": "hello", "v": PROTOCOL_VERSION, "role": self._role})
        answer = self._read_frame()
        if answer.get("ev") == "error":
            raise ProtocolError(answer.get("code", "error"), answer.get("message", ""))
        if answer.get("ev") != "hello" or answer.get("v") != PROTOCOL_VERSION:
            raise ProtocolError("bad-version", f"unexpected greeting {answer}")
        return answer

    def _request(self, op: str, **fields: Any) -> dict[str, Any]:
        req = self._next_req
        self._next_req += 1
        self._send({"op": op, "req": req, **_without_none(fields)})
        return self._await(req)

    def _await(self, req: int) -> dict[str, Any]:
        """Read until the answer to ``req`` arrives.

        Broadcasts carry no ``req`` and may arrive at any time, so they are
        skipped here rather than mistaken for an answer.
        """
        while True:
            frame = self._read_frame()
            if frame.get("req") != req:
                continue
            if frame.get("ev") == "error":
                raise ProtocolError(frame.get("code", "error"), frame.get("message", ""))
            return frame

    # -- browsing --------------------------------------------------------

    def list(
        self,
        limit: int = 50,
        before: Optional[int] = None,
        query: Optional[str] = None,
        kind: Optional[str] = None,
        pinned: Optional[bool] = None,
    ) -> list[Summary]:
        answer = self._request(
            "list", limit=limit, before=before, query=query, kind=kind, pinned=pinned
        )
        return answer["items"]

    def favorites(
        self,
        limit: int = 50,
        before: Optional[int] = None,
        query: Optional[str] = None,
        kind: Optional[str] = None,
    ) -> list[Summary]:
        """Return only starred entries — the durable prompt/snippet store."""
        return self.list(limit=limit, before=before, query=query, kind=kind, pinned=True)

    def fetch(
        self,
        entry: int,
        mime: Optional[str] = None,
        transcode: bool = False,
    ) -> tuple[str, bytes]:
        """Return one representation of an entry as ``(mime, content)``.

        With ``transcode`` the daemon may produce the requested mime when the
        entry does not literally hold it — the one pair defined is
        ``image/*`` → ``image/bmp``, which the RDP clipboard channel relays.
        Anything it cannot produce still raises ``no-such-mime``.
        """
        answer = self._request(
            "fetch",
            entry=entry,
            mime=mime,
            transcode=transcode or None,
        )
        return answer["mime"], self._read_payload(answer["bytes"])

    def thumb(self, entry: int) -> "Thumbnail":
        """Return an entry's thumbnail as decoded RGBA pixels.

        The daemon stores thumbnails compressed and decodes them per request,
        so a client has nothing to decode. ``stride`` is ``width * 4`` with no
        row padding.
        """
        answer = self._request("thumb", entry=entry)
        return Thumbnail(
            width=answer["width"],
            height=answer["height"],
            stride=answer["stride"],
            pixels=self._read_payload(answer["bytes"]),
        )

    def stats(self) -> dict[str, Any]:
        return self._request("stats")

    # -- managing --------------------------------------------------------

    def pin(self, entry: int, pinned: bool = True) -> None:
        self._request("pin", entry=entry, pinned=pinned)

    def remove(self, entry: int) -> None:
        self._request("remove", entry=entry)

    def clear(self) -> None:
        self._request("clear")

    # -- recording -------------------------------------------------------

    def record(
        self,
        parts: Sequence[tuple[str, Any]],
        source: Optional[str] = None,
    ) -> tuple[int, bool]:
        """Record one clipboard event holding every representation in ``parts``.

        Each part's content is either ``bytes``, whose length is declared up
        front, or an iterable of ``bytes``, which is streamed as chunks for a
        source whose length is not known in advance -- a compositor handing
        over a selection, for instance.

        Returns ``(entry id, created)``. ``created`` is false when identical
        content was already archived and only its timestamp moved; the entry
        id is then the existing one.
        """
        begin = self._request("begin", source=source)
        draft = begin["draft"]
        try:
            for mime, content in parts:
                if isinstance(content, (bytes, bytearray, memoryview)):
                    self._whole_part(draft, mime, bytes(content))
                else:
                    self._chunked_part(draft, mime, content)
        except Exception:
            # A draft left open would be aborted on disconnect anyway, but
            # saying so keeps a long-lived capture connection tidy.
            self._request("abort", draft=draft)
            raise

        done = self._request("commit", draft=draft)
        return done["entry"], done["created"]

    def _whole_part(self, draft: int, mime: str, content: bytes) -> None:
        req = self._next_req
        self._next_req += 1
        self._send(
            {
                "op": "part",
                "req": req,
                "draft": draft,
                "mime": mime,
                "bytes": len(content),
            },
            content,
        )
        self._await(req)

    def _chunked_part(self, draft: int, mime: str, chunks: Any) -> None:
        """Stream a part whose total length is not known in advance."""
        req = self._next_req
        self._next_req += 1
        # No `bytes`: the daemon reads chunk frames until a zero-length one.
        self._send({"op": "part", "req": req, "draft": draft, "mime": mime})

        try:
            for chunk in chunks:
                if not chunk:
                    # A zero-length chunk is the terminator, so an empty piece
                    # would end the part early.
                    continue
                self._send({"op": "chunk", "bytes": len(chunk)}, bytes(chunk))
        except Exception as error:
            # The daemon is reading chunks, and every frame after this part is
            # on the far side of the terminator. Leaving it out would strand
            # the connection, so it is sent even though the part is short.
            self._send({"op": "chunk", "bytes": 0})
            # Raised so the caller abandons the draft: what reached the daemon
            # is a truncated representation, and committing it would archive
            # half a picture as though it were whole.
            raise TruncatedPart(mime) from error

        self._send({"op": "chunk", "bytes": 0})
        self._await(req)

    # -- watching --------------------------------------------------------

    def watch(self) -> Iterator[dict[str, Any]]:
        """Yield archive events as they happen.

        Only meaningful for a connection that took the ``ui`` or ``both``
        role; a capture-only connection receives no broadcasts.
        """
        while True:
            frame = self._read_frame()
            if "req" not in frame:
                yield frame


def _without_none(fields: dict[str, Any]) -> dict[str, Any]:
    """Drop unset optional fields so the daemon sees its own defaults."""
    return {key: value for key, value in fields.items() if value is not None}


def _human(count: int) -> str:
    size = float(count)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if size < 1024 or unit == "GiB":
            return f"{size:.0f} {unit}" if unit == "B" else f"{size:.1f} {unit}"
        size /= 1024
    return f"{size:.1f} GiB"


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="rldyour-clipboard",
        description="Read the local rldyour-clipboard archive.",
    )
    sub = parser.add_subparsers(dest="command")

    listing = sub.add_parser("list", help="show recent entries")
    listing.add_argument("-n", "--limit", type=int, default=20)
    listing.add_argument("-q", "--query")
    listing.add_argument("-k", "--kind")
    listing.add_argument(
        "--favorites",
        action="store_true",
        help="only starred entries — the durable prompt store",
    )

    showing = sub.add_parser("get", help="write one entry to standard output")
    showing.add_argument("entry", type=int)
    showing.add_argument("-m", "--mime")

    sub.add_parser("stats", help="show what the archive holds")
    sub.add_parser("watch", help="print archive events as they happen")

    arguments = parser.parse_args(argv)
    command = arguments.command or "list"

    try:
        with Client() as client:
            if command == "list":
                for entry in client.list(
                    limit=arguments.limit,
                    query=arguments.query,
                    kind=arguments.kind,
                    pinned=True if arguments.favorites else None,
                ):
                    pin = "*" if entry["pinned"] else " "
                    preview = entry["preview"] or f"<{entry['kind']}>"
                    print(
                        f"{pin}{entry['id']:>6}  {entry['kind']:<6}"
                        f"  {_human(entry['bytes']):>9}  {preview}"
                    )
            elif command == "get":
                _, content = client.fetch(arguments.entry, arguments.mime)
                sys.stdout.buffer.write(content)
            elif command == "stats":
                report = client.stats()
                print(f"entries: {report['entries']}")
                print(f"favorites: {report['pinned']}")
                print(f"size:    {_human(report['bytes'])}")
                budget = report["budget"]
                print(f"budget:  {'none' if budget >= 2**62 else _human(budget)}")
            elif command == "watch":
                for event in client.watch():
                    print(json.dumps(event, separators=(",", ":")))
    except FileNotFoundError:
        print(
            f"rldyour-clipboard: no daemon at {socket_path()}",
            file=sys.stderr,
        )
        return 1
    except (ProtocolError, ConnectionError) as error:
        print(f"rldyour-clipboard: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
