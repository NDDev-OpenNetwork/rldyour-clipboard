/* rldyour-clipboard — the archive picker
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {setScrollChild, setScrollPolicy, setVertical, verticalBox} from './compat.js';
import {age, iconFor, size, swatch, title} from './format.js';

/** Entries fetched per request. More arrive as the list is scrolled. */
const PAGE = 40;
/** How long typing settles before the archive is searched again. */
const SEARCH_DEBOUNCE_MILLISECONDS = 150;
/** Distance from the bottom at which the next page is fetched, in pixels. */
const PREFETCH_MARGIN = 200;

const FILTERS = [
    {label: 'All', kind: null},
    {label: 'Text', kind: 'text'},
    {label: 'Links', kind: 'link'},
    {label: 'Images', kind: 'image'},
    {label: 'Files', kind: 'files'},
];

/**
 * The list of archived entries, with search, filtering and paging.
 *
 * Everything expensive happens in the daemon. This draws rows from summaries
 * it was handed, fetches a thumbnail as a small PNG when a row needs one, and
 * never reads an entry's content until the user picks it. The archive may hold
 * a hundred thousand entries; what is built here is one window of forty.
 */
export const Picker = GObject.registerClass({
    Signals: {
        /** A row was chosen: `(entry id, paste)`. */
        'activated': {param_types: [GObject.TYPE_INT64, GObject.TYPE_BOOLEAN]},
        /** The picker wants to be dismissed. */
        'dismissed': {},
    },
}, class Picker extends St.BoxLayout {
    _init(client) {
        // The orientation is set afterwards rather than passed in, because
        // the property it lives under differs across the shells supported.
        super._init({styleClass: 'rldyour-picker'});
        setVertical(this);

        this._client = client;
        this._rows = new Map();
        this._query = '';
        this._kind = null;
        this._oldest = null;
        this._exhausted = false;
        this._loading = false;
        this._searchId = 0;
        this._selected = -1;

        this.add_child(this._buildSearch());
        this.add_child(this._buildFilters());
        this.add_child(this._buildList());
        this.add_child(this._buildFooter());
    }

    // -- construction ----------------------------------------------------

    _buildSearch() {
        this._search = new St.Entry({
            styleClass: 'rldyour-search',
            hintText: 'Search what you have copied',
            canFocus: true,
            xExpand: true,
        });
        this._search.set_primary_icon(new St.Icon({
            styleClass: 'rldyour-search-icon',
            iconName: 'edit-find-symbolic',
        }));

        this._search.clutterText.connect('text-changed', () => this._onSearchChanged());
        this._search.clutterText.connect('key-press-event', (_actor, event) =>
            this._onSearchKey(event));

        const box = new St.BoxLayout({styleClass: 'rldyour-search-row'});
        box.add_child(this._search);
        return box;
    }

    _buildFilters() {
        const box = new St.BoxLayout({styleClass: 'rldyour-filters'});
        this._filterButtons = [];

        for (const filter of FILTERS) {
            const button = new St.Button({
                styleClass: 'rldyour-filter',
                label: filter.label,
                canFocus: true,
                toggleMode: true,
            });
            button.checked = filter.kind === null;
            button.connect('clicked', () => this._onFilterClicked(filter.kind));
            box.add_child(button);
            this._filterButtons.push({button, kind: filter.kind});
        }

        return box;
    }

    _buildList() {
        this._list = verticalBox({styleClass: 'rldyour-list'});

        this._empty = new St.Label({
            styleClass: 'rldyour-empty',
            text: 'Nothing here yet. Copy something.',
        });
        this._list.add_child(this._empty);

        this._scroll = new St.ScrollView({
            styleClass: 'rldyour-scroll',
            yExpand: true,
            // The popup is already inside the shell's own scroll handling;
            // overlay scrollbars would sit on top of the rows.
            overlayScrollbars: true,
        });
        setScrollPolicy(this._scroll);
        setScrollChild(this._scroll, this._list);

        // Paging happens on scroll rather than behind a button, so a long
        // archive reads as one list however much of it has been fetched.
        this._adjustment = verticalAdjustment(this._scroll);
        this._adjustment?.connect('notify::value',
            () => this._maybeLoadMore(this._adjustment));

        return this._scroll;
    }

    _buildFooter() {
        const box = new St.BoxLayout({styleClass: 'rldyour-footer'});

        this._summary = new St.Label({
            styleClass: 'rldyour-summary',
            text: '',
            xExpand: true,
            yAlign: Clutter.ActorAlign.CENTER,
        });
        box.add_child(this._summary);

        const clear = new St.Button({
            styleClass: 'rldyour-clear',
            label: 'Clear',
            canFocus: true,
        });
        clear.connect('clicked', () => this._onClear());
        box.add_child(clear);

        return box;
    }

    // -- loading ---------------------------------------------------------

    /** Reloads from the top. Called whenever the picker opens. */
    refresh() {
        this._oldest = null;
        this._exhausted = false;
        this._clearRows();
        this._selected = -1;
        this._load().catch(error => this._report(error));
        this._updateSummary().catch(() => {});
    }

    async _load() {
        if (this._loading || this._exhausted)
            return;
        this._loading = true;

        try {
            const items = await this._client.list({
                limit: PAGE,
                before: this._oldest,
                query: this._query || null,
                kind: this._kind,
            });

            if (items.length < PAGE)
                this._exhausted = true;
            for (const entry of items)
                this._addRow(entry);
            if (items.length > 0)
                this._oldest = items[items.length - 1].id;

            this._empty.visible = this._rows.size === 0;
            this._empty.text = this._query
                ? 'Nothing matches that.'
                : 'Nothing here yet. Copy something.';
        } finally {
            this._loading = false;
        }
    }

    _maybeLoadMore(adjustment) {
        if (!adjustment || this._loading || this._exhausted)
            return;
        const remaining = adjustment.upper - adjustment.pageSize - adjustment.value;
        if (remaining < PREFETCH_MARGIN)
            this._load().catch(error => this._report(error));
    }

    // -- rows ------------------------------------------------------------

    _addRow(entry) {
        const row = new Row(entry, this._client);
        row.connect('activated', (_row, paste) => {
            this.emit('activated', entry.id, paste);
        });
        row.connect('pin-toggled', () => this._onPin(entry));
        row.connect('removed', () => this._onRemove(entry));

        this._rows.set(entry.id, row);
        this._list.add_child(row);
    }

    _clearRows() {
        for (const row of this._rows.values())
            row.destroy();
        this._rows.clear();
    }

    /** Applies an archive broadcast without reloading the whole list. */
    onArchiveEvent(event) {
        switch (event.ev) {
        case 'added':
            // Only when looking at the unfiltered top of the list: inserting
            // into a search result the entry may not match would be a lie.
            if (!this._query && !this._kind && !this._rows.has(event.entry.id)) {
                const row = new Row(event.entry, this._client);
                row.connect('activated', (_row, paste) =>
                    this.emit('activated', event.entry.id, paste));
                row.connect('pin-toggled', () => this._onPin(event.entry));
                row.connect('removed', () => this._onRemove(event.entry));
                this._rows.set(event.entry.id, row);
                this._list.insert_child_at_index(row, 1);
                this._empty.visible = false;
            }
            break;
        case 'removed':
            this._rows.get(event.entry)?.destroy();
            this._rows.delete(event.entry);
            this._empty.visible = this._rows.size === 0;
            break;
        case 'cleared':
            this.refresh();
            break;
        }
        this._updateSummary().catch(() => {});
    }

    // -- interaction -----------------------------------------------------

    _onSearchChanged() {
        const text = this._search.get_text().trim();
        if (text === this._query)
            return;
        this._query = text;

        // Debounced: a search per keystroke would ask the daemon for a new
        // window of the archive on every letter.
        if (this._searchId)
            GLib.Source.remove(this._searchId);
        this._searchId = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
            SEARCH_DEBOUNCE_MILLISECONDS, () => {
                this._searchId = 0;
                this.refresh();
                return GLib.SOURCE_REMOVE;
            });
    }

    _onSearchKey(event) {
        const symbol = event.get_key_symbol();

        switch (symbol) {
        case Clutter.KEY_Escape:
            if (this._search.get_text() !== '') {
                // The first Escape clears the search; a second dismisses.
                this._search.set_text('');
                return Clutter.EVENT_STOP;
            }
            this.emit('dismissed');
            return Clutter.EVENT_STOP;

        case Clutter.KEY_Down:
            this._move(1);
            return Clutter.EVENT_STOP;

        case Clutter.KEY_Up:
            this._move(-1);
            return Clutter.EVENT_STOP;

        case Clutter.KEY_Return:
        case Clutter.KEY_KP_Enter: {
            const rows = [...this._rows.values()];
            const row = rows[this._selected >= 0 ? this._selected : 0];
            if (row) {
                // Shift pastes without the shortcut, for somewhere the paste
                // keystroke would be wrong.
                const paste = (event.get_state() & Clutter.ModifierType.SHIFT_MASK) === 0;
                this.emit('activated', row.entryId, paste);
            }
            return Clutter.EVENT_STOP;
        }

        default:
            return Clutter.EVENT_PROPAGATE;
        }
    }

    _move(delta) {
        const rows = [...this._rows.values()];
        if (rows.length === 0)
            return;

        if (this._selected >= 0)
            rows[this._selected]?.setHighlighted(false);
        this._selected = Math.max(0, Math.min(rows.length - 1, this._selected + delta));
        const row = rows[this._selected];
        row.setHighlighted(true);
        // Taking the key focus is also what scrolls the row into view: an
        // St.ScrollView follows focus on its own.
        row.grab_key_focus();
    }

    _onFilterClicked(kind) {
        this._kind = kind;
        for (const {button, kind: own} of this._filterButtons)
            button.checked = own === kind;
        this.refresh();
    }

    _onPin(entry) {
        this._client.pin(entry.id, !entry.pinned)
            .then(() => {
                entry.pinned = !entry.pinned;
                // Pinning changes the sort order, so the list is rebuilt
                // rather than patched in place.
                this.refresh();
            })
            .catch(error => this._report(error));
    }

    _onRemove(entry) {
        this._client.remove(entry.id).catch(error => this._report(error));
    }

    _onClear() {
        this._client.clear().catch(error => this._report(error));
    }

    async _updateSummary() {
        const stats = await this._client.stats();
        const budget = stats.budget >= Number.MAX_SAFE_INTEGER
            ? 'no limit'
            : size(stats.budget);
        this._summary.text =
            `${stats.entries} ${stats.entries === 1 ? 'entry' : 'entries'} · ` +
            `${size(stats.bytes)} of ${budget}`;
    }

    _report(error) {
        console.debug(`rldyour-clipboard: ${error}`);
    }

    /** Puts the keyboard in the search box, which is where typing belongs. */
    focusSearch() {
        global.stage.set_key_focus(this._search.clutterText);
    }

    reset() {
        this._search.set_text('');
        this._query = '';
    }

    destroy() {
        if (this._searchId)
            GLib.Source.remove(this._searchId);
        this._searchId = 0;
        this._clearRows();
        super.destroy();
    }
});

/**
 * The vertical adjustment of a scroll view, whichever way this shell exposes
 * it: GNOME 46 has the `vscroll` child bar, later versions only the getter.
 */
function verticalAdjustment(scrollView) {
    if (typeof scrollView.get_vadjustment === 'function')
        return scrollView.get_vadjustment();
    return scrollView.vscroll?.adjustment ?? null;
}

/**
 * One archived entry.
 *
 * A row shows what the summary already says. The one thing it fetches is a
 * thumbnail, and only for an entry the daemon has already made one for — so
 * drawing the list never decodes an image in this process.
 */
const Row = GObject.registerClass({
    Signals: {
        'activated': {param_types: [GObject.TYPE_BOOLEAN]},
        'pin-toggled': {},
        'removed': {},
    },
}, class Row extends St.Button {
    _init(entry, client) {
        super._init({
            styleClass: 'rldyour-row',
            canFocus: true,
            xExpand: true,
        });

        this.entryId = entry.id;
        this._entry = entry;
        this._client = client;
        this._cancellable = new Gio.Cancellable();

        const box = new St.BoxLayout({styleClass: 'rldyour-row-box'});
        box.add_child(this._buildGlyph(entry));
        box.add_child(this._buildText(entry));
        box.add_child(this._buildActions(entry));
        this.set_child(box);

        this.connect('clicked', () => this.emit('activated', true));
        this.connect('destroy', () => this._cancellable.cancel());

        if (entry.thumb)
            this._loadThumbnail();
    }

    _buildGlyph(entry) {
        const colour = entry.kind === 'color' ? swatch(entry.preview) : null;
        if (colour) {
            this._glyph = new St.Widget({styleClass: 'rldyour-swatch'});
            this._glyph.set_style(`background-color: ${colour};`);
            return this._glyph;
        }

        this._glyph = new St.Icon({
            styleClass: 'rldyour-glyph',
            iconName: iconFor(entry.kind),
            yAlign: Clutter.ActorAlign.CENTER,
        });
        return this._glyph;
    }

    _buildText(entry) {
        const box = verticalBox({
            styleClass: 'rldyour-row-text',
            xExpand: true,
            yAlign: Clutter.ActorAlign.CENTER,
        });

        this._title = new St.Label({
            styleClass: 'rldyour-row-title',
            text: title(entry),
        });
        this._title.clutterText.singleLineMode = true;
        this._title.clutterText.ellipsize = Pango.EllipsizeMode.END;
        box.add_child(this._title);

        const details = [age(entry.at), size(entry.bytes)];
        if (entry.source)
            details.push(entry.source);
        box.add_child(new St.Label({
            styleClass: 'rldyour-row-detail',
            text: details.join(' · '),
        }));

        return box;
    }

    _buildActions(entry) {
        const box = new St.BoxLayout({
            styleClass: 'rldyour-row-actions',
            yAlign: Clutter.ActorAlign.CENTER,
        });

        this._pin = new St.Button({
            styleClass: entry.pinned ? 'rldyour-action rldyour-pinned' : 'rldyour-action',
            canFocus: true,
            child: new St.Icon({
                iconName: entry.pinned ? 'starred-symbolic' : 'non-starred-symbolic',
                styleClass: 'rldyour-action-icon',
            }),
        });
        this._pin.connect('clicked', () => {
            this.emit('pin-toggled');
            return Clutter.EVENT_STOP;
        });
        box.add_child(this._pin);

        const remove = new St.Button({
            styleClass: 'rldyour-action',
            canFocus: true,
            child: new St.Icon({
                iconName: 'user-trash-symbolic',
                styleClass: 'rldyour-action-icon',
            }),
        });
        remove.connect('clicked', () => {
            this.emit('removed');
            return Clutter.EVENT_STOP;
        });
        box.add_child(remove);

        return box;
    }

    /**
     * Replaces the kind icon with the daemon's thumbnail.
     *
     * The bytes are a PNG of at most 256 pixels that the daemon already made,
     * handed to the shell's texture cache as a loadable icon. Decoding an
     * original image here is exactly what the daemon exists to prevent.
     */
    _loadThumbnail() {
        this._client.thumb(this._entry.id)
            .then(bytes => {
                if (this._cancellable.is_cancelled())
                    return;
                this._glyph.styleClass = 'rldyour-thumb';
                this._glyph.gicon = Gio.BytesIcon.new(bytes);
            })
            .catch(() => {
                // An entry removed between the list and the thumbnail, or one
                // whose thumbnail is gone. The kind icon is already showing.
            });
    }

    setHighlighted(on) {
        if (on)
            this.add_style_class_name('rldyour-row-selected');
        else
            this.remove_style_class_name('rldyour-row-selected');
    }
});
