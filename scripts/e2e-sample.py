#!/usr/bin/env python3
"""End-to-end check against a running daemon.

Records entries of several shapes, reads them back and verifies the archive
behaved as `docs/protocol.md` says it must. Exits non-zero on the first
disagreement, so CI can run it unattended.
"""

from __future__ import annotations

import io
import struct
import sys
import zlib

from rldyour_clipboard import Client, ProtocolError


def png(width: int, height: int) -> bytes:
    """A minimal valid PNG, built here so the test carries no fixture."""

    def chunk(tag: bytes, body: bytes) -> bytes:
        return (
            struct.pack(">I", len(body))
            + tag
            + body
            + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + b"\x7f\x00\x40" * width for _ in range(height))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def check(condition: bool, what: str) -> None:
    print(f"  {'ok  ' if condition else 'FAIL'} {what}")
    if not condition:
        sys.exit(1)


def main() -> int:
    with Client(role="both") as client:
        check(client.hello["v"] == 1, "daemon greets with protocol version 1")

        print("Text with two representations")
        entry, created = client.record(
            [("text/html", b"<b>git rebase -i</b>"), ("text/plain", b"git rebase -i")],
            source="e2e",
        )
        check(created, "a new entry was created")
        mime, content = client.fetch(entry)
        check(mime == "text/html", "the richest representation is served by default")
        check(content == b"<b>git rebase -i</b>", "content round-trips byte for byte")
        mime, content = client.fetch(entry, "text/plain")
        check(content == b"git rebase -i", "a named representation is served as asked")

        print("Deduplication")
        again, created_again = client.record([("text/plain", b"git rebase -i"),
                                              ("text/html", b"<b>git rebase -i</b>")])
        check(again == entry, "the same content in another order is the same entry")
        check(not created_again, "a repeated copy creates nothing")

        print("An image, with a thumbnail made by the daemon")
        picture = png(600, 300)
        image_entry, _ = client.record([("image/png", picture)], source="e2e")
        summary = next(e for e in client.list(limit=50) if e["id"] == image_entry)
        check(summary["kind"] == "image", "an image entry is classified as one")
        check((summary["width"], summary["height"]) == (600, 300), "dimensions recorded")
        check(summary["thumb"], "a thumbnail is available")
        thumbnail = client.thumb(image_entry)
        # Served as pixels, not a container: a client that may be a compositor
        # must not have to decode anything.
        check(thumbnail.width == 256, "the thumbnail is scaled into the box")
        check(thumbnail.height == 128, "and keeps the aspect ratio")
        check(thumbnail.stride == thumbnail.width * 4, "rows are tightly packed")
        check(
            len(thumbnail.pixels) == thumbnail.stride * thumbnail.height,
            "the payload is exactly the pixels it declared",
        )
        _, served = client.fetch(image_entry)
        check(served == picture, "the original image is served untouched")

        # The RDP clipboard channel relays image/bmp and no other image type,
        # so a fetch may ask the daemon to produce it by transcoding.
        mime, converted = client.fetch(image_entry, "image/bmp", transcode=True)
        check(mime == "image/bmp", "transcoding serves the mime asked for")
        check(
            converted[:2] == b"BM" and len(converted) > 54,
            "the transcoded image is a real BMP file",
        )
        try:
            client.fetch(entry, "text/x-nonesuch", transcode=True)
            check(False, "a mime the daemon cannot produce is still an error")
        except ProtocolError as error:
            check(
                error.code == "no-such-mime",
                "untranscodable requests still answer no-such-mime",
            )

        print("A payload far larger than any control frame")
        big = bytes((i * 7) % 256 for i in range(3_000_000))
        big_entry, _ = client.record([("application/octet-stream", big)])
        _, served = client.fetch(big_entry)
        check(served == big, "a 3 MB entry round-trips byte for byte")

        print("A part streamed without a declared length")
        pieces = [bytes([i % 256]) * 100_000 for i in range(12)]
        streamed_entry, _ = client.record(
            [("application/octet-stream", iter(pieces))], source="e2e-chunked"
        )
        _, served = client.fetch(streamed_entry)
        check(served == b"".join(pieces), "a chunked 1.2 MB part round-trips")
        whole_entry, whole_created = client.record(
            [("application/octet-stream", b"".join(pieces))]
        )
        check(
            whole_entry == streamed_entry and not whole_created,
            "chunked and whole delivery of the same bytes are the same entry",
        )
        client.remove(streamed_entry)

        print("Secrets")
        secret, secret_created = client.record(
            [("text/plain", b"hunter2"), ("x-kde-passwordManagerHint", b"secret")],
            source="keepassxc",
        )
        check(secret == 0 and not secret_created, "a password-hinted entry is refused")
        check(
            all(e["preview"] != "hunter2" for e in client.list(limit=200)),
            "the refused content is nowhere in the archive",
        )

        print("Search")
        found = client.list(query="rebase")
        check(any(e["id"] == entry for e in found), "search finds the text entry")
        check(
            client.list(query='"') is not None,
            "punctuation in a query is searched for, not executed",
        )

        print("Pinning and removal")
        client.pin(entry, True)
        listed = client.list(limit=50)
        check(listed[0]["id"] == entry, "a pinned entry sorts first")

        starred = client.favorites()
        check(
            any(e["id"] == entry for e in starred),
            "the favorites list holds the starred entry",
        )
        check(
            all(e["id"] != entry for e in client.list(limit=50, pinned=False)),
            "favorites stay out of the unpinned stream",
        )
        check(client.stats()["pinned"] >= 1, "stats counts favorites")
        client.pin(entry, False)
        check(
            all(e["id"] != entry for e in client.favorites()),
            "unpinning removes the entry from favorites",
        )

        client.remove(big_entry)
        try:
            client.fetch(big_entry)
            check(False, "a removed entry cannot be fetched")
        except ProtocolError as error:
            check(error.code == "no-such-entry", "a removed entry answers no-such-entry")

        print("Errors")
        try:
            client.fetch(entry, "application/nonesuch")
            check(False, "an absent representation is an error")
        except ProtocolError as error:
            check(error.code == "no-such-mime", "an absent mime answers no-such-mime")

        try:
            # Text has no thumbnail, but the entry is there.
            client.thumb(entry)
            check(False, "a missing thumbnail is an error")
        except ProtocolError as error:
            check(
                error.code == "no-such-mime",
                "a missing thumbnail answers no-such-mime, not no-such-entry",
            )

        report = client.stats()
        check(report["entries"] >= 2, "stats counts what is held")
        print(f"\nArchive holds {report['entries']} entries, {report['bytes']} bytes")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
