/* rldyour-clipboard — companion daemon client
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {FrameReader, sliceBytes} from './framing.js';

const SOCKET_NAME = 'rldyour-clipboard.sock';
const PROTOCOL_VERSION = 1;

/** Refuse a control line long enough to mean the peer is not our daemon. */
const MAX_FRAME = 64 * 1024;
/** How much of a stream is forwarded per chunk frame. */
const CHUNK = 64 * 1024;

const RECONNECT_MIN_SECONDS = 1;
const RECONNECT_MAX_SECONDS = 30;

Gio._promisify(Gio.SocketClient.prototype, 'connect_async');
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
        this._writing = Promise.resolve();

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
        // RLDYOUR_CLIPBOARD_HOME relocates archive and socket together on
        // every platform; the runtime directory is the production default.
        const root = GLib.getenv('RLDYOUR_CLIPBOARD_HOME')
            ?? GLib.get_user_runtime_dir();
        const path = GLib.build_filenamev([root, SOCKET_NAME]);
        const connection = await new Gio.SocketClient().connect_async(
            new Gio.UnixSocketAddress({path}), this._cancellable);

        if (this._stopped) {
            connection.close_async(GLib.PRIORITY_DEFAULT, null, null);
            return;
        }
        this._connection = connection;
        this._output = connection.get_output_stream();
        this._input = new FrameReader(connection.get_input_stream(), this._cancellable);

        // The greeting is positional: it is the one answer without a request
        // id, so it is read here rather than through the dispatch loop.
        await this._write({op: 'hello', v: PROTOCOL_VERSION, role: this._role});
        const greeting = await this._readFrame();
        if (greeting?.ev !== 'hello' || greeting.v !== PROTOCOL_VERSION)
            throw new Error(`unexpected greeting from the daemon: ${JSON.stringify(greeting)}`);

        this._hello = greeting;
        this._backoff = RECONNECT_MIN_SECONDS;
        this._onState(true);
        this._dispatch(connection, this._input).catch(() => this._retry(connection));
    }

    /** Reads answers and broadcasts until the connection ends. */
    async _dispatch(connection, input) {
        for (;;) {
            const frame = await input.frame();
            if (connection !== this._connection || this._stopped) return;
            if (frame === null) {
                this._retry(connection);
                return;
            }

            // A payload follows its frame immediately and belongs to the
            // request that asked for it.
            let payload = null;
            if (frame.ev === 'blob' || frame.ev === 'thumb')
                payload = await input.payload(frame.bytes);
            if (connection !== this._connection || this._stopped) return;

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

    _readFrame() { return this._input.frame(); }
    _readPayload(count) { return this._input.payload(count); }

    async _writeAll(bytes) {
        const output = this._output;
        const connection = this._connection;
        if (output === null) throw new Error('not connected');
        let offset = 0;
        while (offset < bytes.get_size()) {
            const remaining = sliceBytes(bytes, offset, bytes.get_size() - offset);
            if (connection !== this._connection || this._stopped) throw new Error('clipboard connection changed');
            const written = await output.write_bytes_async(remaining,
                GLib.PRIORITY_DEFAULT, this._cancellable);
            if (written <= 0)
                throw new Error('clipboard socket stopped accepting bytes');
            offset += written;
        }
    }

    _enqueue(write) {
        const connection = this._connection;
        const operation = this._writing.then(() => {
            if (connection !== this._connection || this._stopped)
                throw new Error('clipboard connection changed');
            return write();
        });
        this._writing = operation.catch(() => {});
        return operation;
    }

    async _write(frame, payload = null) {
        if (this._output === null)
            throw new Error('not connected to the daemon');

        const line = new GLib.Bytes(`${JSON.stringify(frame)}\n`);
        if (line.get_size() > MAX_FRAME)
            throw new Error('outgoing clipboard control frame is too large');
        await this._writeAll(line);
        if (payload !== null) {
            await this._writeAll(payload);
        }
    }

    /** Sends a request and resolves with its answer. */
    async _request(op, fields = {}) {
        const req = this._nextReq++;
        const answer = new Promise((resolve, reject) => {
            this._pending.set(req, {resolve, reject});
        });
        try {
            await this._enqueue(() => this._write({op, req, ...fields}));
        } catch (error) {
            this._pending.delete(req);
            throw error;
        }
        return answer;
    }

    // -- browsing --------------------------------------------------------

    async list({limit = 50, before = null, query = null, kind = null, pinned = null} = {}) {
        const fields = {limit};
        if (before !== null)
            fields.before = before;
        if (query)
            fields.query = query;
        if (kind)
            fields.kind = kind;
        if (pinned !== null)
            fields.pinned = pinned;
        const answer = await this._request('list', fields);
        Object.defineProperty(answer.items, 'more', {value: answer.more ?? answer.items.length === limit});
        return answer.items;
    }

    /**
     * Returns `{mime, bytes}` for one representation of an entry.
     *
     * `transcode` lets the daemon produce the requested mime when the entry
     * does not literally hold it — the one pair defined is `image/*` →
     * `image/bmp`, which is the only image type the RDP clipboard channel
     * relays. Anything it cannot produce still answers `no-such-mime`.
     */
    async fetch(entry, mime = null, transcode = false) {
        const fields = {entry};
        if (mime !== null)
            fields.mime = mime;
        if (transcode)
            fields.transcode = true;
        const {frame, payload} = await this._request('fetch', fields);
        return {mime: frame.mime, bytes: payload};
    }

    /**
     * Returns a thumbnail as `{width, height, stride, bytes}`.
     *
     * `bytes` is straight RGBA the daemon has already decoded, which is what
     * `St.ImageContent.set_bytes` wants and is the only image data a shell
     * extension can draw without decoding something itself.
     */
    async thumb(entry) {
        const {frame, payload} = await this._request('thumb', {entry});
        return {
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            bytes: payload,
        };
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
        const connection = this._connection;
        const write = (frame, payload = null) => {
            if (connection !== this._connection) throw new Error('clipboard connection changed');
            return this._write(frame, payload);
        };
        const req = this._nextReq++;
        const answer = new Promise((resolve, reject) => {
            this._pending.set(req, {resolve, reject});
        });

        await this._enqueue(async () => {
        try {
            // No `bytes`: the daemon reads chunks until a zero-length one.
            await write({op: 'part', req, draft, mime});
        } catch (error) {
            this._pending.delete(req);
            throw error;
        }

        try {
            for (;;) {
                const chunk = await stream.read_bytes_async(
                    CHUNK, GLib.PRIORITY_DEFAULT, this._cancellable);
                const size = chunk.get_size();
                if (size === 0)
                    break;
                await write({op: 'chunk', bytes: size}, chunk);
            }
        } catch (error) {
            this._pending.delete(req);
            // The daemon is reading chunks, and every frame after this part is
            // on the far side of the terminator. Leaving it out would strand
            // the connection, so it is sent even though the part is now short.
            await write({op: 'chunk', bytes: 0}).catch(() => {});
            // Rethrown so the caller aborts the draft: what reached the daemon
            // is a truncated representation, and committing it would archive a
            // half a picture as though it were whole.
            throw new TruncatedPart(mime, error);
        }

        await write({op: 'chunk', bytes: 0});
        });
        return answer;
    }

    // -- lifecycle -------------------------------------------------------

    _retry(connection = this._connection) {
        if (this._stopped || connection !== this._connection)
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

/**
 * A representation that was only partly sent.
 *
 * Distinct from any other failure because of what it means for the draft: the
 * daemon holds a short copy of this representation, so the entry must be
 * abandoned rather than committed.
 */
export class TruncatedPart extends Error {
    constructor(mime, cause) {
        super(`the ${mime} representation was cut off: ${cause}`);
        this.mime = mime;
        this.cause = cause;
    }
}

/** An error the daemon named, as opposed to one the transport raised. */
export class DaemonError extends Error {
    constructor(code, message) {
        super(`${code}: ${message}`);
        this.code = code;
    }
}
