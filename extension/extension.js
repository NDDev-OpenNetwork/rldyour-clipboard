/* rldyour-clipboard — GNOME Shell extension
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {Capture} from './lib/capture.js';
import {Client} from './lib/client.js';
import {Indicator} from './lib/indicator.js';
import {Restore} from './lib/restore.js';

/**
 * The tray box, and a position just left of the indicators already in it.
 *
 * The AppIndicator extension adds its icons to this same box at index 1, so a
 * lower index puts this one alongside them rather than somewhere else on the
 * panel.
 */
const PANEL_BOX = 'right';
const PANEL_POSITION = 0;

const SHORTCUT = 'open-picker';

/**
 * Wiring only.
 *
 * Capture reads the selection, the client carries it to the daemon, the
 * indicator draws the archive and restore puts an entry back. This file exists
 * to connect those four and to take them all down again, because an extension
 * that leaks one object survives a disable and then misbehaves after the next
 * enable.
 */
export default class ClipboardExtension extends Extension {
    enable() {
        this._settings = this.getSettings();

        this._client = new Client({
            role: 'both',
            onEvent: event => this._onArchiveEvent(event),
            onState: connected => this._indicator?.setConnected(connected),
        });

        this._capture = new Capture(this._client, this._settings);
        this._restore = new Restore(this._client, this._capture, this._settings);

        this._indicator = new Indicator(this._client, this._settings);
        this._indicator.connect('activated', (_indicator, id, paste) =>
            this._onActivated(id, paste));
        Main.panel.addToStatusArea(this.uuid, this._indicator, PANEL_POSITION, PANEL_BOX);

        Main.wm.addKeybinding(
            SHORTCUT,
            this._settings,
            Meta.KeyBindingFlags.NONE,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._indicator?.toggle());
    }

    disable() {
        // The shell may call disable without a completed enable — when a lock
        // screen interrupts startup, for instance — so every teardown is
        // guarded rather than assumed.
        Main.wm.removeKeybinding(SHORTCUT);

        this._indicator?.destroy();
        this._indicator = null;

        this._restore?.destroy();
        this._restore = null;

        this._capture?.destroy();
        this._capture = null;

        // Last: the other three hold references to it.
        this._client?.stop();
        this._client = null;

        this._settings = null;
    }

    _onArchiveEvent(event) {
        this._indicator?.onArchiveEvent(event);
    }

    _onActivated(id, paste) {
        // The picker hands over an id rather than a summary, because by the
        // time the panel closes the entry may have been evicted under it.
        this._client.list({limit: 1, before: id + 1})
            .then(([entry]) => {
                if (entry?.id !== id)
                    return;
                return this._restore.activate(entry, {paste});
            })
            .catch(error => {
                console.debug(`rldyour-clipboard: could not restore entry ${id}: ${error}`);
            });
    }
}
