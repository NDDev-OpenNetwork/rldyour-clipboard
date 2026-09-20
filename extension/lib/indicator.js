/* rldyour-clipboard — the tray indicator
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import GObject from 'gi://GObject';
import St from 'gi://St';

import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {Picker} from './picker.js';

/**
 * The tray icon and the panel it opens.
 *
 * ## Why this is not a StatusNotifierItem
 *
 * The icons beside this one — a chat client, an assistant — are
 * StatusNotifierItems drawn by the AppIndicator extension. This is not one,
 * deliberately. That extension maps a single left click to *showing a menu*
 * after a double-click timeout, and reserves `Activate` for a double click;
 * an item with no menu gets nothing at all on a single click. So an SNI item
 * cannot open a window on one click, whatever it does.
 *
 * A panel button placed in the same box lands in the same row as those icons,
 * looks like one of them, and owns its own click. That is the whole reason for
 * the choice.
 */
export const Indicator = GObject.registerClass({
    // Named explicitly. Left to GJS the GType would be derived from the file
    // path and the class name -- `Gjs_lib_indicator_Indicator` -- which every
    // other extension with a `lib/indicator.js` exporting an `Indicator` also
    // claims. The second one to load then fails outright with "already
    // registered", losing the whole extension rather than one widget.
    GTypeName: 'RldyourClipboardIndicator',
    Signals: {
        /** A row was chosen: `(entry summary, paste)`. */
        'activated': {param_types: [GObject.TYPE_JSOBJECT, GObject.TYPE_BOOLEAN]},
    },
}, class Indicator extends PanelMenu.Button {
    _init(client, settings) {
        super._init(0.5, 'rldyour clipboard', false);

        this._client = client;
        this._settings = settings;

        this._icon = new St.Icon({
            styleClass: 'system-status-icon rldyour-indicator-icon',
            iconName: 'edit-paste-symbolic',
        });
        this.add_child(this._icon);

        this._picker = new Picker(client);
        this._picker.connect('activated', (_picker, entry, paste) => {
            // Closing first is what hands the keyboard back to the window the
            // user was typing in; the paste is typed once it has landed.
            this.menu.close(true);
            this.emit('activated', entry, paste);
        });
        this._picker.connect('dismissed', () => this.menu.close(true));

        const section = new PopupMenu.PopupMenuSection();
        section.actor.add_child(this._picker);
        this.menu.addMenuItem(section);
        this.menu.box.add_style_class_name('rldyour-menu');

        this._openStateId = this.menu.connect('open-state-changed',
            (_menu, open) => this._onOpenStateChanged(open));

        this.setConnected(client.connected);
    }

    _onOpenStateChanged(open) {
        // Destroying the button closes the menu, which emits this signal one
        // last time — after _picker may already be gone.
        if (!this._picker)
            return;

        if (!open) {
            this._picker.reset();
            return;
        }

        this._picker.refresh();
        // Typing goes to the search box from the moment the panel appears,
        // so finding an entry never needs a click first.
        this._picker.focusSearch();
    }

    /** Applies an archive broadcast to the list, when it is showing. */
    onArchiveEvent(event) {
        if (this.menu.isOpen)
            this._picker.onArchiveEvent(event);
    }

    /**
     * Dims the icon while the daemon is away.
     *
     * The daemon is socket-activated and allowed to exit when nobody is
     * watching, so this is a state rather than an error, and the icon says so
     * without a notification.
     */
    setConnected(connected) {
        this._icon.opacity = connected ? 255 : 100;
        this.accessible_name = connected
            ? 'Clipboard history'
            : 'Clipboard history (the archive daemon is not running)';
    }

    /** Opens the panel, for the keyboard shortcut. */
    open() {
        this.menu.open(true);
    }

    toggle() {
        this.menu.toggle();
    }

    destroy() {
        if (this._openStateId)
            this.menu.disconnect(this._openStateId);
        this._openStateId = 0;
        this._picker?.destroy();
        this._picker = null;
        super.destroy();
    }
});
