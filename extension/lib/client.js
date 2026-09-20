/* rldyour-clipboard — companion daemon client
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const SOCKET_NAME = 'rldyour-clipboard.sock';
const PROTOCOL_VERSION = 1;

/** Refuse a control line long enough to mean the peer is not our daemon. */
const MAX_FRAME = 64 * 1024;
/** How much of a stream is forwarded per chunk frame. */
const CHUNK = 64 * 1024;

const RECONNECT_MIN_SECONDS = 1;
const RECONNECT_MAX_SECONDS = 30;

Gio._promisify(Gio.SocketClient.prototype, 'connect_async');
Gio._promisify(Gio.DataInputStream.prototype, 'read_line_async', 'read_line_finish_utf8');
Gio._promisify(Gio.InputStream.prototype, 'read_bytes_async');
Gio._promisify(Gio.OutputStream.prototype, 'write_bytes_async');

/**
 * Speaks the daemon's protocol from inside the shell process.
 *
 * Every method that talks to the daemon is asynchronous and none of them ever
 * blocks: this code runs on the compositor's own thread, where a synchronous
 * read of an archive entry would freeze the desktop for as long as it took.
 *
 * The daemon answers a connection's requests in the order they arrived, so
 * pending requests are matched by their id and broadcasts — which carry none —
 * are handed to the listener instead.
 */
export class Client {
    /**
     * @param {object} options
     * @param {string} options.role 'capture', 'ui' or 'both'
     * @param {(event: object) => void} [options.onEvent] archive broadcasts
     * @param {(connected: boolean) => void} [options.onState] link changes
     */
    constructor({role = 'ui', onEvent = () => {}, onState = () => {}} = {}) {
        this._role = role;
        this._onEvent = onEvent;
        this._onState = onState;

        this._cancellable = new Gio.Cancellable();
        this._connection = null;
        this._input = null;
        this._output = null;
        this._pending = new Map();
        this._nextReq = 1;
        this._reconnectId = 0;
        this._backoff = RECONNECT_MIN_SECONDS;
        this._stopped = false;
        this._hello = null;

        this._connect().catch(() => this._retry());
    }

    get connected() {
        return this._connection !== null;
    }

    /** The archive's shape as of the last handshake, or null when offline. */
    get archive() {
        return this._hello;
    }

    async _connect() {
        const path = GLib.build_filenamev([GLib.get_user_runtime_dir(), SOCKET_NAME]);
        const connection = await new Gio.SocketClient().connect_async(
            new Gio.UnixSocketAddress({path}), this._cancellable);

        this._connection = connection;
        this._output = connection.get_output_stream();
        this._input = new Gio.DataInputStream({
            baseStream: connection.get_input_stream(),
            // The daemon may hand over a blob of any size; the buffer only has
            // to be large enough for a control frame.
            bufferSize: MAX_FRAME,
        });

        // The greeting is positional: it is the one answer without a request
        // id, so it is read here rather than through the dispatch loop.
        await this._write({op: 'hello', v: PROTOCOL_VERSION, role: this._role});
        const greeting = await this._readFrame();
        if (greeting?.ev !== 'hello' || greeting.v !== PROTOCOL_VERSION)
            throw new Error(`unexpected greeting from the daemon: ${JSON.stringify(greeting)}`);

        this._hello = greeting;
        this._backoff = RECONNECT_MIN_SECONDS;
        this._onState(true);
        this._dispatch().catch(() => this._retry());
    }

    /** Reads answers and broadcasts until the connection ends. */
    async _dispatch() {
        for (;;) {
            const frame = await this._readFrame();
            if (frame === null) {
                this._retry();
                return;
            }

            // A blob's payload follows its frame immediately and belongs to
            // the request that asked for it.
            let payload = null;
            if (frame.ev === 'blob')
                payload = await this._readPayload(frame.bytes);

            if (frame.req === undefined) {
                this._onEvent(frame);
                continue;
            }

            const pending = this._pending.get(frame.req);
            if (pending === undefined)
                continue;
            this._pending.delete(frame.req);

            if (frame.ev === 'error')
                pending.reject(new DaemonError(frame.code, frame.message));
            else
                pending.resolve(payload === null ? frame : {frame, payload});
        }
    }

    async _readFrame() {
        const [line] = await this._input.read_line_async(GLib.PRIORITY_DEFAULT, this._cancellable);
        // A null line is a clean close, which is how the daemon says it is
        // going away because nobody needed it.
        if (line === null)
            return null;
        if (line.length > MAX_FRAME)
            throw new Error('the peer sent a control frame longer than the protocol defines');

        try {
            return JSON.parse(line);
        } catch {
            throw new Error('the peer sent something that is not a control frame');
        }
    }

    async _readPayload(count) {
        const parts = [];
        let remaining = count;

        // read_bytes_async is allowed to return short, so a payload is read to
        // its declared length rather than in one call.
        while (remaining > 0) {
            const chunk = await this._input.read_bytes_async(
                Math.min(remaining, CHUNK), GLib.PRIORITY_DEFAULT, this._cancellable);
            const size = chunk.get_size();
            if (size === 0)
                throw new Error('the payload ended early');
            parts.push(chunk);
            remaining -= size;
        }

        if (parts.length === 1)
            return parts[0];

        const joined = new Uint8Array(count);
        let at = 0;
        for (const part of parts) {
            joined.set(part.toArray(), at);
            at += part.get_size();
        }
        return new GLib.Bytes(joined);
    }

    async _write(frame, payload = null) {
        if (this._output === null)
            throw new Error('not connected to the daemon');

        const line = `${JSON.stringify(frame)}\n`;
        await this._output.write_bytes_async(
            new GLib.Bytes(line), GLib.PRIORITY_DEFAULT, this._cancellable);
        if (payload !== null) {
            await this._output.write_bytes_async(
                payload, GLib.PRIORITY_DEFAULT, this._cancellable);
        }
    }

    /** Sends a request and resolves with its answer. */
    async _request(op, fields = {}) {
        const req = this._nextReq++;
        const answer = new Promise((resolve, reject) => {
            this._pending.set(req, {resolve, reject});
        });
        try {
            await this._write({op, req, ...fields});
        } catch (error) {
            this._pending.delete(req);
            throw error;
        }
        return answer;
    }

    // -- browsing --------------------------------------------------------

    async list({limit = 50, before = null, query = null, kind = null} = {}) {
        const fields = {limit};
        if (before !== null)
            fields.before = before;
        if (query)
            fields.query = query;
        if (kind)
            fields.kind = kind;
        const answer = await this._request('list', fields);
        return answer.items;
    }

    /** Returns `{mime, bytes}` for one representation of an entry. */
    async fetch(entry, mime = null) {
        const fields = mime === null ? {entry} : {entry, mime};
        const {frame, payload} = await this._request('fetch', fields);
        return {mime: frame.mime, bytes: payload};
    }

    async thumb(entry) {
        const {payload} = await this._request('thumb', {entry});
        return payload;
    }

    pin(entry, pinned) {
        return this._request('pin', {entry, pinned});
    }

    remove(entry) {
        return this._request('remove', {entry});
    }

    clear() {
        return this._request('clear');
    }

    stats() {
        return this._request('stats');
    }

    // -- recording -------------------------------------------------------

    async begin(source = null) {
        const answer = await this._request('begin', source ? {source} : {});
        return answer.draft;
    }

    async commit(draft) {
        return this._request('commit', {draft});
    }

    abort(draft) {
        return this._request('abort', {draft});
    }

    /**
     * Streams one representation straight from `stream` to the daemon.
     *
     * The length is never known in advance — a compositor hands a selection
     * over as a stream and never says how long it is — so the part is sent as
     * chunks. Nothing larger than one chunk is ever held in this process,
     * which is the whole reason the shell can carry an entry of any size.
     */
    async part(draft, mime, stream) {
        const req = this._nextReq++;
        const answer = new Promise((resolve, reject) => {
            this._pending.set(req, {resolve, reject});
        });

        try {
            // No `bytes`: the daemon reads chunks until a zero-length one.
            await this._write({op: 'part', req, draft, mime});

            for (;;) {
                const chunk = await stream.read_bytes_async(
                    CHUNK, GLib.PRIORITY_DEFAULT, this._cancellable);
                const size = chunk.get_size();
                if (size === 0)
                    break;
                await this._write({op: 'chunk', bytes: size}, chunk);
            }
            await this._write({op: 'chunk', bytes: 0});
        } catch (error) {
            this._pending.delete(req);
            throw error;
        }

        return answer;
    }

    // -- lifecycle -------------------------------------------------------

    _retry() {
        if (this._stopped)
            return;

        this._teardown();
        this._onState(false);

        if (this._reconnectId)
            GLib.Source.remove(this._reconnectId);
        this._reconnectId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT, this._backoff, () => {
                this._reconnectId = 0;
                this._connect().catch(() => this._retry());
                return GLib.SOURCE_REMOVE;
            });

        // The daemon is socket-activated and allowed to be absent, so backing
        // off keeps a stopped service from costing a wakeup a second.
        this._backoff = Math.min(this._backoff * 2, RECONNECT_MAX_SECONDS);
    }

    _teardown() {
        for (const pending of this._pending.values())
            pending.reject(new DaemonError('closed', 'the connection to the daemon ended'));
        this._pending.clear();

        if (this._connection)
            this._connection.close_async(GLib.PRIORITY_DEFAULT, null, null);
        this._connection = null;
        this._input = null;
        this._output = null;
        this._hello = null;
    }

    /** Releases the socket and the reconnect timer. The client is done after this. */
    stop() {
        this._stopped = true;
        if (this._reconnectId)
            GLib.Source.remove(this._reconnectId);
        this._reconnectId = 0;

        this._cancellable.cancel();
        this._teardown();
    }
}

/** An error the daemon named, as opposed to one the transport raised. */
export class DaemonError extends Error {
    constructor(code, message) {
        super(`${code}: ${message}`);
        this.code = code;
    }
}
