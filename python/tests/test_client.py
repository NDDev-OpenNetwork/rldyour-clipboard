"""Tests for the protocol client.

A stub daemon on a real socket rather than a mocked one: the framing — a JSON
line, then a declared count of raw bytes — is the thing worth testing, and a
mock would test the mock.
"""

from __future__ import annotations

import json
import socket
import tempfile
import threading
from pathlib import Path

import pytest

from rldyour_clipboard import Client, ProtocolError


class StubDaemon:
    """Answers one connection from a scripted list of replies."""

    def __init__(self, replies):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "sock"
        self.received: list[dict] = []
        self.payloads: list[bytes] = []
        self._replies = list(replies)

        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(str(self.path))
        self._server.listen(1)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        connection, _ = self._server.accept()
        reader = connection.makefile("rb")
        try:
            while True:
                line = reader.readline()
                if not line:
                    return
                frame = json.loads(line)
                self.received.append(frame)
                # A frame that declares a payload is followed by exactly that
                # many bytes, so the stub has to consume them to stay aligned.
                count = frame.get("bytes")
                if count:
                    self.payloads.append(reader.read(count))
                if self._replies:
                    connection.sendall(self._replies.pop(0))
        finally:
            connection.close()

    def close(self) -> None:
        self._server.close()
        self.directory.cleanup()


def frame(obj: dict) -> bytes:
    return json.dumps(obj).encode() + b"\n"


HELLO = frame({"ev": "hello", "v": 1, "entries": 0, "bytes": 0, "budget": 1024})


@pytest.fixture
def daemon(request):
    stub = StubDaemon(request.param if hasattr(request, "param") else [HELLO])
    yield stub
    stub.close()


def test_greets_with_the_protocol_version_and_role():
    stub = StubDaemon([HELLO])
    try:
        with Client(path=stub.path, role="capture") as client:
            assert client.hello["v"] == 1
        assert stub.received[0] == {"op": "hello", "v": 1, "role": "capture"}
    finally:
        stub.close()


def test_a_daemon_speaking_another_version_is_refused():
    stub = StubDaemon([frame({"ev": "hello", "v": 99})])
    try:
        with pytest.raises(ProtocolError) as raised:
            Client(path=stub.path)
        assert raised.value.code == "bad-version"
    finally:
        stub.close()


def test_an_error_answer_becomes_an_exception_carrying_its_code():
    stub = StubDaemon([
        HELLO,
        frame({"ev": "error", "req": 1, "code": "no-such-entry", "message": "gone"}),
    ])
    try:
        with Client(path=stub.path) as client:
            with pytest.raises(ProtocolError) as raised:
                client.fetch(42)
            assert raised.value.code == "no-such-entry"
    finally:
        stub.close()


def test_a_blob_payload_is_read_to_its_declared_length():
    content = bytes(range(256)) * 40
    stub = StubDaemon([
        HELLO,
        frame({"ev": "blob", "req": 1, "mime": "image/png", "bytes": len(content)})
        + content,
    ])
    try:
        with Client(path=stub.path) as client:
            mime, read = client.fetch(7)
        assert mime == "image/png"
        # Byte for byte: a payload is never decoded or re-encoded.
        assert read == content
    finally:
        stub.close()


def test_broadcasts_do_not_get_mistaken_for_an_answer():
    stub = StubDaemon([
        HELLO,
        # An event arrives before the answer to the request in flight.
        frame({"ev": "added", "entry": {"id": 1}})
        + frame({"ev": "list", "req": 1, "items": [{"id": 5}]}),
    ])
    try:
        with Client(path=stub.path) as client:
            items = client.list()
        assert items == [{"id": 5}]
    finally:
        stub.close()


def test_a_whole_part_declares_its_length_up_front():
    stub = StubDaemon([
        HELLO,
        frame({"ev": "begin", "req": 1, "draft": 3}),
        frame({"ev": "ok", "req": 2}),
        frame({"ev": "commit", "req": 3, "entry": 9, "created": True}),
    ])
    try:
        with Client(path=stub.path, role="capture") as client:
            entry, created = client.record([("text/plain", b"hello")], source="test")
        assert (entry, created) == (9, True)

        part = next(f for f in stub.received if f.get("op") == "part")
        assert part["bytes"] == 5
        assert stub.payloads == [b"hello"]
    finally:
        stub.close()


def test_a_part_of_unknown_length_is_chunked_and_terminated():
    stub = StubDaemon([
        HELLO,
        frame({"ev": "begin", "req": 1, "draft": 3}),
        frame({"ev": "ok", "req": 2}),
        frame({"ev": "commit", "req": 3, "entry": 9, "created": True}),
    ])
    try:
        with Client(path=stub.path, role="capture") as client:
            client.record([("application/octet-stream", iter([b"one", b"two"]))])

        part = next(f for f in stub.received if f.get("op") == "part")
        # No length: that is what puts the daemon into chunked mode.
        assert "bytes" not in part

        chunks = [f for f in stub.received if f.get("op") == "chunk"]
        assert [c["bytes"] for c in chunks] == [3, 3, 0]
        assert stub.payloads == [b"one", b"two"]
    finally:
        stub.close()


def test_an_empty_piece_does_not_end_a_chunked_part_early():
    stub = StubDaemon([
        HELLO,
        frame({"ev": "begin", "req": 1, "draft": 3}),
        frame({"ev": "ok", "req": 2}),
        frame({"ev": "commit", "req": 3, "entry": 9, "created": False}),
    ])
    try:
        with Client(path=stub.path, role="capture") as client:
            client.record([("text/plain", iter([b"a", b"", b"b"]))])

        chunks = [c["bytes"] for c in stub.received if c.get("op") == "chunk"]
        # The empty piece is skipped; only the terminator is zero.
        assert chunks == [1, 1, 0]
    finally:
        stub.close()


def test_optional_fields_left_unset_are_not_sent():
    stub = StubDaemon([HELLO, frame({"ev": "list", "req": 1, "items": []})])
    try:
        with Client(path=stub.path) as client:
            client.list(limit=10)
        listing = next(f for f in stub.received if f.get("op") == "list")
        # Sending `"query": null` would ask the daemon to search for nothing
        # rather than to leave the search out.
        assert listing == {"op": "list", "req": 1, "limit": 10}
    finally:
        stub.close()
