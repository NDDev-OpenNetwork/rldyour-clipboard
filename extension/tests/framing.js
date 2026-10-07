import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {FrameReader} from '../lib/framing.js';
const loop = new GLib.MainLoop(null, false);
let error = null;
async function test() {
    const bytes = new TextEncoder().encode('{"ev":"blob","bytes":6}\nabc\n{}{"ev":"ok","req":1}\n');
    let reader = new FrameReader(Gio.MemoryInputStream.new_from_bytes(new GLib.Bytes(bytes)), new Gio.Cancellable());
    if ((await reader.frame()).bytes !== 6) throw new Error('frame');
    if ((await reader.payload(6)).get_size() !== 6) throw new Error('payload');
    if ((await reader.frame()).req !== 1 || await reader.frame() !== null) throw new Error('next frame');
    let rejected = false;
    reader = new FrameReader(Gio.MemoryInputStream.new_from_bytes(new GLib.Bytes(new Uint8Array(65538).fill(120))), new Gio.Cancellable());
    try { await reader.frame(); } catch { rejected = true; }
    if (!rejected) throw new Error('oversized frame accepted');
    const length = 1024 * 1024 + 16;
    const payload = new Uint8Array(length).fill(123);
    reader = new FrameReader(Gio.MemoryInputStream.new_from_bytes(new GLib.Bytes(payload)), new Gio.Cancellable());
    const mapped = await reader.payload(length);
    if (mapped.get_size() !== length || mapped.get_data()[0] !== 123) throw new Error('mapped payload');
    const leftovers = Gio.File.new_for_path(GLib.get_user_runtime_dir()).enumerate_children('standard::name', Gio.FileQueryInfoFlags.NONE, null);
    for (let item = leftovers.next_file(null); item; item = leftovers.next_file(null)) {
        if (item.get_name().startsWith('.rldyour-clipboard-')) throw new Error('transfer file leaked');
    }
    leftovers.close(null);
    print('PASS: frame/payload boundaries, bounded headers, mapped large payload and cleanup');
}
test().catch(e => { error = e; }).finally(() => loop.quit());
loop.run();
if (error) throw error;
