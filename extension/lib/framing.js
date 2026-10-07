/* Bounded control lines and binary payloads share one buffered input. */
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const MAX_FRAME = 64 * 1024;
const CHUNK = 64 * 1024;
const DECODER = new TextDecoder('utf-8', {fatal: true});
Gio._promisify(Gio.InputStream.prototype, 'read_bytes_async', 'read_bytes_finish');
Gio._promisify(Gio.File.prototype, 'create_async', 'create_finish');
Gio._promisify(Gio.OutputStream.prototype, 'write_bytes_async', 'write_bytes_finish');
Gio._promisify(Gio.OutputStream.prototype, 'close_async', 'close_finish');

export class FrameReader {
    constructor(input, cancellable) {
        this._input = input;
        this._cancellable = cancellable;
        this._buffer = new Uint8Array(0);
    }

    async frame() {
        for (;;) {
            const end = this._buffer.indexOf(10);
            if (end >= 0) {
                if (end > MAX_FRAME)
                    throw new Error('oversized clipboard control frame');
                const value = JSON.parse(DECODER.decode(this._buffer.subarray(0, end)));
                this._buffer = this._buffer.slice(end + 1);
                if (value === null || typeof value !== 'object' || Array.isArray(value))
                    throw new Error('clipboard control frame must be an object');
                return value;
            }
            if (this._buffer.length > MAX_FRAME)
                throw new Error('unterminated clipboard control frame');
            const bytes = await this._input.read_bytes_async(
                Math.min(4096, MAX_FRAME + 1 - this._buffer.length),
                GLib.PRIORITY_DEFAULT, this._cancellable);
            const chunk = bytes.get_data();
            if (chunk.length === 0) {
                if (this._buffer.length)
                    throw new Error('truncated clipboard control frame');
                return null;
            }
            const joined = new Uint8Array(this._buffer.length + chunk.length);
            joined.set(this._buffer);
            joined.set(chunk, this._buffer.length);
            this._buffer = joined;
        }
    }

    async payload(count) {
        if (!Number.isSafeInteger(count) || count < 0 || count > 512 * 1024 * 1024)
            throw new Error('clipboard restore payload exceeds the desktop client limit');
        if (count > 1024 * 1024)
            return this._mappedPayload(count);
        const result = new Uint8Array(count);
        let offset = 0;
        if (this._buffer.length) {
            const take = Math.min(count, this._buffer.length);
            result.set(this._buffer.subarray(0, take));
            this._buffer = this._buffer.slice(take);
            offset = take;
        }
        while (offset < count) {
            const bytes = await this._input.read_bytes_async(
                Math.min(CHUNK, count - offset), GLib.PRIORITY_DEFAULT, this._cancellable);
            if (!bytes.get_size())
                throw new Error('clipboard payload ended early');
            result.set(bytes.get_data(), offset);
            offset += bytes.get_size();
        }
        return new GLib.Bytes(result);
    }

    async _mappedPayload(count) {
        const path = GLib.build_filenamev([GLib.get_user_runtime_dir(),
            `.rldyour-clipboard-${GLib.uuid_string_random()}`]);
        const file = Gio.File.new_for_path(path);
        let output = null;
        try {
            output = await file.create_async(Gio.FileCreateFlags.PRIVATE,
                GLib.PRIORITY_DEFAULT, this._cancellable);
            const write = async bytes => {
                let offset = 0;
                while (offset < bytes.get_size()) {
                    const remaining = GLib.Bytes.new_from_bytes(bytes, offset, bytes.get_size() - offset);
                    const n = await output.write_bytes_async(remaining,
                        GLib.PRIORITY_DEFAULT, this._cancellable);
                    if (n <= 0)
                        throw new Error('clipboard transfer file stopped accepting data');
                    offset += n;
                }
            };
            let received = 0;
            if (this._buffer.length) {
                const take = Math.min(count, this._buffer.length);
                await write(new GLib.Bytes(this._buffer.subarray(0, take)));
                this._buffer = this._buffer.slice(take);
                received = take;
            }
            while (received < count) {
                const bytes = await this._input.read_bytes_async(
                    Math.min(CHUNK, count - received), GLib.PRIORITY_DEFAULT, this._cancellable);
                if (!bytes.get_size())
                    throw new Error('clipboard payload ended early');
                await write(bytes);
                received += bytes.get_size();
            }
            await output.close_async(GLib.PRIORITY_DEFAULT, this._cancellable);
            output = null;
            // Bytes owns the mapping; unlinking the private transfer file
            // leaves no restore cache and never copies the whole payload into
            // the compositor's JavaScript heap.
            return GLib.MappedFile.new(path, false).get_bytes();
        } finally {
            if (output)
                output.close_async(GLib.PRIORITY_DEFAULT, null).catch(() => {});
            try { file.delete(null); } catch { /* Already gone or cancelled. */ }
        }
    }
}
