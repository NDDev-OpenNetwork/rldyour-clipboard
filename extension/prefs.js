/* rldyour-clipboard — preferences
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Runs in its own process, not the shell's. Nothing here may import St,
 * Clutter, Meta or Shell: doing so crashes the host process rather than
 * failing.
 */

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

export default class ClipboardPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        const page = new Adw.PreferencesPage({
            title: 'Clipboard',
            iconName: 'edit-paste-symbolic',
        });
        window.add(page);

        page.add(this._archiveGroup(settings));
        page.add(this._pasteGroup(settings));
        page.add(this._exclusionGroup(settings));
    }

    _archiveGroup(settings) {
        const group = new Adw.PreferencesGroup({
            title: 'Archive',
            description:
                'The archive itself — how much it keeps and where — belongs to the ' +
                'daemon, and is set in its service file rather than here.',
        });

        const record = new Adw.SwitchRow({
            title: 'Remember what is copied',
            subtitle: 'Turning this off keeps the archive but stops adding to it',
        });
        settings.bind('record', record, 'active', Gio.SettingsBindFlags.DEFAULT);
        group.add(record);

        const limit = new Adw.SpinRow({
            title: 'Largest item to keep',
            subtitle: 'Megabytes. Anything bigger is skipped, not truncated',
            adjustment: new Gtk.Adjustment({
                lower: 1,
                upper: 8192,
                stepIncrement: 16,
                pageIncrement: 128,
            }),
        });
        settings.bind('max-entry-megabytes', limit, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(limit);

        return group;
    }

    _pasteGroup(settings) {
        const group = new Adw.PreferencesGroup({title: 'Pasting'});

        const paste = new Adw.SwitchRow({
            title: 'Paste as soon as an item is chosen',
            subtitle: 'Otherwise the item is only put on the clipboard',
        });
        settings.bind('paste-on-select', paste, 'active', Gio.SettingsBindFlags.DEFAULT);
        group.add(paste);

        group.add(this._listRow(
            settings,
            'terminal-apps',
            'Applications that paste with Ctrl+Shift+V',
            'One window class per line. Empty uses the built-in list of terminals'));

        return group;
    }

    _exclusionGroup(settings) {
        const group = new Adw.PreferencesGroup({
            title: 'Exclusions',
            description:
                'Password managers are already excluded whatever is listed here, ' +
                'because they mark their clipboard content as a secret.',
        });

        group.add(this._listRow(
            settings,
            'excluded-apps',
            'Never remember copies from',
            'One window class per line, matched without regard to case'));

        return group;
    }

    /**
     * A multi-line text view bound to a string-list setting.
     *
     * Written out rather than taken from a list widget because these lists are
     * short, hand-edited and occasionally pasted in whole.
     */
    _listRow(settings, key, title, subtitle) {
        const row = new Adw.ActionRow({title, subtitle});

        const view = new Gtk.TextView({
            monospace: true,
            topMargin: 6,
            bottomMargin: 6,
            leftMargin: 6,
            rightMargin: 6,
        });
        const scrolled = new Gtk.ScrolledWindow({
            heightRequest: 96,
            widthRequest: 260,
            marginTop: 8,
            marginBottom: 8,
            child: view,
            hasFrame: true,
        });

        view.buffer.text = settings.get_strv(key).join('\n');
        view.buffer.connect('changed', buffer => {
            const text = buffer.get_text(
                buffer.get_start_iter(), buffer.get_end_iter(), false);
            const entries = text
                .split('\n')
                .map(line => line.trim())
                .filter(line => line.length > 0);
            settings.set_strv(key, entries);
        });

        row.add_suffix(scrolled);
        return row;
    }
}
