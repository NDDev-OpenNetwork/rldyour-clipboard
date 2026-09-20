/* rldyour-clipboard — putting an entry back on the clipboard
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';

import {preferredMime} from './mimes.js';

/**
 * Applications whose paste shortcut is Ctrl+Shift+V rather than Ctrl+V.
 *
 * In a terminal Ctrl+V is the literal-next-character escape, so sending it
 * would insert a control code instead of pasting. The list is a default the
 * user can extend; an application not on it gets the ordinary shortcut.
 */
const DEFAULT_TERMINALS = [
    'gnome-terminal-server',
    'org.gnome.terminal',
    'org.gnome.console',
    'kgx',
    'konsole',
    'xterm',
    'alacritty',
    'kitty',
    'wezterm',
    'foot',
    'terminator',
    'tilix',
    'xfce4-terminal',
    'ptyxis',
];

/**
 * How long to wait after closing the picker before typing the paste.
 *
 * Closing a menu hands the keyboard back to the window underneath, and that
 * happens on the next main loop iteration. Typing before it completes would
 * send the shortcut into a window that is no longer listening.
 */
const FOCUS_SETTLE_MILLISECONDS = 60;

/**
 * Serves archive entries back to the rest of the desktop.
 *
 * ## One representation at a time
 *
 * An entry may hold several representations, but only one can be offered back.
 * `MetaSelectionSourceMemory` is the only selection source mutter exposes, and
 * it carries a single mime type; serving several would need a custom
 * `MetaSelectionSource`, which cannot be written in an extension because GJS
 * refuses to implement a vfunc that takes a callback — `read_async` is exactly
 * that.
 *
 * So the representation is chosen, and chosen to fail safe. Text is restored
 * as `text/plain` even when the entry also holds `text/html`: plain text
 * pasted into a rich editor is merely unstyled, whereas HTML offered to a
 * terminal or a search field matches nothing at all and pastes nothing.
 * Images and file references have no such ambiguity and are restored as
 * themselves. `restore` takes an explicit mime for the times the user wants
 * the other one.
 */
export class Restore {
    /**
     * @param {import('./client.js').Client} client connected to the daemon
     * @param {import('./capture.js').Capture} capture told about our own writes
     * @param {Gio.Settings} settings the extension's settings
     */
    constructor(client, capture, settings) {
        this._client = client;
        this._capture = capture;
        this._settings = settings;
        this._pasteId = 0;
    }

    /**
     * Puts one entry on the clipboard and, if asked, pastes it.
     *
     * @param {object} entry the summary the picker is showing
     * @param {object} [options]
     * @param {string} [options.mime] a representation other than the default
     * @param {boolean} [options.paste] type the paste shortcut afterwards
     */
    async activate(entry, {mime = null, paste = true} = {}) {
        const wanted = mime ?? preferredMime(entry);
        const {mime: served, bytes} = await this._client.fetch(entry.id, wanted);

        const source = Meta.SelectionSourceMemory.new(served, bytes);

        // Told before the owner changes, because `set_owner` raises
        // `owner-changed` synchronously and the capture must recognise this
        // source as its own or it would archive the paste as a fresh copy.
        this._capture?.setOurs(source);
        global.display.get_selection().set_owner(
            Meta.SelectionType.SELECTION_CLIPBOARD, source);

        if (paste && this._settings.get_boolean('paste-on-select'))
            this._paste();
    }

    /**
     * Types the paste shortcut into whatever had the keyboard before.
     *
     * Deferred by a moment because the picker has only just closed: the
     * window underneath gets the keyboard back on a later main loop
     * iteration, and a shortcut sent before that would go nowhere.
     */
    _paste() {
        if (this._pasteId)
            GLib.Source.remove(this._pasteId);

        this._pasteId = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
            FOCUS_SETTLE_MILLISECONDS, () => {
                this._pasteId = 0;
                try {
                    type(this._shortcutFor(focusedApplication()));
                } catch (error) {
                    console.debug(`rldyour-clipboard: could not type the paste: ${error}`);
                }
                return GLib.SOURCE_REMOVE;
            });
    }

    _shortcutFor(application) {
        if (!application)
            return {shift: false};
        const terminals = this._settings.get_strv('terminal-apps');
        const known = terminals.length > 0 ? terminals : DEFAULT_TERMINALS;
        const shift = known.some(name => name.toLowerCase() === application.toLowerCase());
        return {shift};
    }

    destroy() {
        if (this._pasteId)
            GLib.Source.remove(this._pasteId);
        this._pasteId = 0;
    }
}

/** Sends the paste shortcut through the compositor's own virtual keyboard. */
function type({shift}) {
    const seat = Clutter.get_default_backend().get_default_seat();
    const keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);

    // Microseconds: what the virtual device's clock is in.
    const when = global.get_current_time() * 1000;
    const {PRESSED, RELEASED} = Clutter.KeyState;

    const held = [Clutter.KEY_Control_L];
    if (shift)
        held.push(Clutter.KEY_Shift_L);

    for (const key of held)
        keyboard.notify_keyval(when, key, PRESSED);
    keyboard.notify_keyval(when, Clutter.KEY_v, PRESSED);
    keyboard.notify_keyval(when, Clutter.KEY_v, RELEASED);
    // Released in reverse so the modifier state unwinds the way a real
    // keyboard would report it.
    for (const key of held.reverse())
        keyboard.notify_keyval(when, key, RELEASED);
}

function focusedApplication() {
    const window = global.display.focus_window;
    if (!window)
        return null;
    return window.get_wm_class_instance() ?? window.get_wm_class() ?? null;
}
