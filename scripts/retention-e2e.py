#!/usr/bin/env python3
"""Exercise startup retention and durable pins in a fresh isolated archive.
Only synthetic content is used. Native capture is disabled explicitly.
"""
import os
import pathlib
import sqlite3
import subprocess
import tempfile
import time
from rldyour_clipboard import Client

binary = pathlib.Path(__file__).resolve().parents[1] / 'daemon/target/release/rldyour-clipboardd'
with tempfile.TemporaryDirectory(prefix='cb-', dir='/tmp') as root:
    root = str(pathlib.Path(root).resolve())
    env = dict(os.environ, RLDYOUR_CLIPBOARD_HOME=root, RLDYOUR_CLIPBOARD_CAPTURE='0')
    for key in ('LISTEN_PID', 'LISTEN_FDS', 'LISTEN_FDNAMES'):
        env.pop(key, None)
    path = pathlib.Path(root) / 'rldyour-clipboard.sock'
    def start():
        process = subprocess.Popen([str(binary)], env=env, stdout=subprocess.DEVNULL)
        for _ in range(200):
            try:
                with Client(path=path, watch=False):
                    return process
            except (OSError, RuntimeError):
                if process.poll() is not None:
                    raise RuntimeError('isolated daemon exited')
                time.sleep(0.025)
        process.terminate();process.wait(timeout=5)
        raise RuntimeError('isolated daemon did not start')
    def stop(process):
        process.terminate();process.wait(timeout=5)
    process = start()
    try:
        with Client(path=path, role='both', watch=False) as client:
            old, _ = client.record([('text/plain', b'expire synthetic')])
            kept, _ = client.record([('text/plain', b'keep synthetic')])
            fresh, _ = client.record([('text/plain', b'fresh synthetic')])
            client.pin(kept)
    finally:
        stop(process)
    with sqlite3.connect(pathlib.Path(root) / 'index.db') as db:
        db.execute('UPDATE entry SET at=? WHERE id IN (?,?)', (int(time.time()) - 8 * 86400, old, kept))
    process = start()
    try:
        with Client(path=path, watch=False) as client:
            ids = {entry['id'] for entry in client.list()}
            assert old not in ids and kept in ids and fresh in ids
            assert client.favorites()[0]['id'] == kept
            assert client.fetch(kept)[1] == b'keep synthetic'
            assert client.stats()['retention_days'] == 7
        print('PASS: unpinned >7 days removed at startup; pin and contents survive daemon restart')
    finally:
        stop(process)
