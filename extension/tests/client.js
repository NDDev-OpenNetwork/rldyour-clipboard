import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {Client} from '../lib/client.js';
const loop = new GLib.MainLoop(null, false);
let error = null;
let running = false;
const client = new Client({role: 'both', onState: connected => {
    if (!connected || running) return;
    running = true;
    test().catch(e => { error = e; }).finally(() => { client.stop(); loop.quit(); });
}});
async function test() {
    const draft = await client.begin('synthetic-gjs');
    const content = new GLib.Bytes(new TextEncoder().encode('synthetic clipboard GJS protocol test'));
    const input = Gio.MemoryInputStream.new_from_bytes(content);
    const upload = client.part(draft, 'text/plain', input);
    const statsDuringTransfer = client.stats();
    await upload;
    await statsDuringTransfer;
    const committed = await client.commit(draft);
    await client.pin(committed.entry, true);
    const favorites = await client.list({pinned: true});
    if (!favorites.some(entry => entry.id === committed.entry && entry.pinned)) throw new Error('pin missing');
    const restored = await client.fetch(committed.entry, 'text/plain');
    if (restored.bytes.get_size() !== content.get_size()) throw new Error('restore mismatch');
    if ((await client.stats()).retention_days !== 7) throw new Error('retention metadata');
    await client.remove(committed.entry);
    print('PASS: GJS handshake, serialized upload+query, pin/favorites, restore and metadata');
}
const timeout = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 15, () => { error = new Error('client timeout'); client.stop(); loop.quit(); return GLib.SOURCE_REMOVE; });
loop.run();
GLib.Source.remove(timeout);
if (error) throw error;
