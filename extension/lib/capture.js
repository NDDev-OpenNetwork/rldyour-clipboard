/* rldyour-clipboard — clipboard capture
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';

Gio._promisify(Meta.Selection.prototype, 'transfer_async', 'transfer_finish');

import {anySensitive, recordable} from './mimes.js';

/** How much of one representation may be staged before it is abandoned. */
const DEFAULT_MAX_MEGABYTES = 512;

/**
 * Records clipboard events into the archive.
 *
 * The compositor owns the selection, so this is the one piece that must run
 * inside the shell process. It is kept to exactly that: it reads no content
 * into this process, hashes nothing, decodes nothing and keeps no history.
 * Each representation is transferred from the selection into a staging file
 * and streamed from there to the daemon in chunks, so the shell's memory never
 * grows with the size of what was copied.
 *
 * The staging file is the price of GJS having no usable pipe binding — the
 * selection must be spliced into a `GOutputStream`, and the only one that can
 * be drained afterwards without buffering it all is a file. It lives in the
 * per-user runtime directory, is created private, and is deleted as soon as it
 * has been forwarded.
 */
export class Capture {
    /**
     * @param {import('./client.js').Client} client connected to the daemon
     * @param {Gio.Settings} settings the extension's settings
     */
    constructor(client, settings) {
        this._client = client;
        this._settings = settings;
        this._selection = global.display.get_selection();
        this._cancellable = new Gio.Cancellable();

        /** Set while this extension is writing the clipboard itself. */
        this._ours = null;
        /** Serialises events so two quick copies cannot interleave. */
        this._queue = Promise.resolve();
        this._staged = 0;

        this._ownerChangedId = this._selection.connect('owner-changed',
            (_selection, type, source) => this._onOwnerChanged(type, source));
    }

    /**
     * Marks a selection source as this extension's own.
     *
     * Restoring an entry makes the shell the selection owner, which raises
     * `owner-changed` exactly as a real copy would. Comparing the owner by
     * identity is what stops a paste from being recorded as a new entry, and
     * it is exact — no timing window and no content comparison.
     */
    setOurs(source) {
        this._ours = source;
    }

    _onOwnerChanged(type, source) {
        if (type !== Meta.SelectionType.SELECTION_CLIPBOARD)
            return;
        // Our own restore. Recording it would add a duplicate every paste.
        if (source !== null && source === this._ours)
            return;
        if (!this._settings.get_boolean('record'))
            return;

        const application = focusedApplication();
        if (this._isExcluded(application))
            return;

        // Each event is queued rather than run at once: a source that asserts
        // the clipboard twice in quick succession must not have its two
        // transfers interleaved into one draft.
        this._queue = this._queue
            .then(() => this._record(application))
            .catch(error => {
                if (!error.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    console.debug(`rldyour-clipboard: could not record a clipboard event: ${error}`);
            });
    }

    _isExcluded(application) {
        if (!application)
            return false;
        const excluded = this._settings.get_strv('excluded-apps');
        return excluded.some(name => name.toLowerCase() === application.toLowerCase());
    }

    async _record(application) {
        const type = Meta.SelectionType.SELECTION_CLIPBOARD;
        const offered = this._selection.get_mimetypes(type) ?? [];
        if (offered.length === 0)
            return;

        // One secret hint condemns the whole event, whatever else it offers.
        if (anySensitive(offered))
            return;

        const wanted = recordable(offered);
        if (wanted.length === 0)
            return;
        if (!this._client.connected)
            return;

        const draft = await this._client.begin(application);
        let recorded = 0;
        try {
            for (const mime of wanted) {
                if (await this._recordOne(draft, type, mime))
                    recorded += 1;
            }
        } catch (error) {
            await this._client.abort(draft).catch(() => {});
            throw error;
        }

        if (recorded === 0) {
            await this._client.abort(draft).catch(() => {});
            return;
        }
        await this._client.commit(draft);
    }

    /**
     * Transfers one representation out of the selection and into the archive.
     *
     * A representation that fails is skipped rather than fatal: a source that
     * advertises a target it cannot actually produce is common, and it must
     * not cost the entry its other representations.
     *
     * @returns {Promise<boolean>} whether it was recorded
     */
    async _recordOne(draft, type, mime) {
        const staging = this._stagingFile();

        try {
            const target = staging.replace(null, false, Gio.FileCreateFlags.PRIVATE,
                this._cancellable);
            // -1 means "no limit": the compositor splices the whole selection
            // in, which is what lets an entry be any size at all.
            await this._selection.transfer_async(type, mime, -1, target, this._cancellable);

            const size = staging.query_info('standard::size',
                Gio.FileQueryInfoFlags.NONE, this._cancellable).get_size();
            if (size === 0)
                return false;
            if (size > this._maximumBytes()) {
                console.debug(`rldyour-clipboard: skipping ${mime}, ${size} bytes is over the limit`);
                return false;
            }

            const source = staging.read(this._cancellable);
            try {
                await this._client.part(draft, mime, source);
            } finally {
                source.close(null);
            }
            return true;
        } catch (error) {
            if (error.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                throw error;
            console.debug(`rldyour-clipboard: could not read ${mime}: ${error}`);
            return false;
        } finally {
            try {
                staging.delete(null);
            } catch {
                // Already gone, or never created. The runtime directory is
                // cleared at logout either way.
            }
        }
    }

    _stagingFile() {
        const name = `rldyour-clipboard-stage-${Gio.Application.get_default()?.application_id ?? 'shell'}-${this._staged++}`;
        return Gio.File.new_for_path(
            GLib.build_filenamev([GLib.get_user_runtime_dir(), name]));
    }

    _maximumBytes() {
        const megabytes = this._settings.get_int('max-entry-megabytes') || DEFAULT_MAX_MEGABYTES;
        return megabytes * 1024 * 1024;
    }

    destroy() {
        if (this._ownerChangedId)
            this._selection.disconnect(this._ownerChangedId);
        this._ownerChangedId = 0;
        this._cancellable.cancel();
        this._ours = null;
    }
}

/** The application the clipboard most likely came from, as a hint for the UI. */
function focusedApplication() {
    const window = global.display.focus_window;
    if (!window)
        return null;
    return window.get_wm_class_instance() ?? window.get_wm_class() ?? null;
}
