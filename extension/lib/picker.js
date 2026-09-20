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

import {
    setImageBytes,
    setScrollPolicy,
    setVertical,
    verticalAdjustment,
    verticalBox,
} from './compat.js';
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
 * The two pages the picker shows.
 *
 * Recent is the live stream — the newest copies, ten at a time. Favorites is
 * the durable store: starred entries the daemon never evicts, so a prompt or
 * an image kept there survives restarts and reboots. They are disjoint on
 * purpose — starring an entry moves it off the stream onto the page meant to
 * hold it.
 */
const TABS = [
    {label: 'Recent', pinned: false, pageSize: 10},
    {label: 'Favorites', pinned: true, pageSize: PAGE},
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
    // Explicit for the same reason as the indicator: a GType name derived
    // from the path collides with any extension laid out the same way.
    GTypeName: 'RldyourClipboardPicker',
    Signals: {
        /** A row was chosen: `(entry summary, paste)`. */
        'activated': {param_types: [GObject.TYPE_JSOBJECT, GObject.TYPE_BOOLEAN]},
        /** The picker wants to be dismissed. */
        'dismissed': {},
    },
}, class Picker extends St.BoxLayout {
    _init(client) {
        // The orientation is set afterwards rather than passed in, because the
        // property it lives under differs across the shells supported.
        super._init({styleClass: 'rldyour-picker'});
        setVertical(this);

        this._client = client;
        this._rows = new Map();
        this._tab = TABS[0];
        this._query = '';
        this._kind = null;
        this._oldest = null;
        this._exhausted = false;
        this._loading = false;
        this._searchId = 0;
        this._selected = -1;

        this.add_child(this._buildSearch());
        this.add_child(this._buildTabs());
        this.add_child(this._buildFilters());
        // The placeholder is a sibling of the list rather than its first
        // child, so inserting a new row is always an insert at zero.
        this._empty = new St.Label({
            styleClass: 'rldyour-empty',
            text: 'Nothing here yet. Copy something.',
        });
        this.add_child(this._empty);
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

    _buildTabs() {
        const box = new St.BoxLayout({styleClass: 'rldyour-tabs'});
        this._tabButtons = [];

        for (const tab of TABS) {
            const button = new St.Button({
                styleClass: 'rldyour-tab',
                label: tab.label,
                canFocus: true,
                toggleMode: true,
                xExpand: true,
            });
            button.checked = tab === this._tab;
            button.connect('clicked', () => this._onTabClicked(tab));
            box.add_child(button);
            this._tabButtons.push({button, tab});
        }

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

        this._scroll = new St.ScrollView({
            styleClass: 'rldyour-scroll',
            yExpand: true,
            // The rows already sit in a narrow popup; a scrollbar taking
            // width from them would cost a character of every preview.
            overlayScrollbars: true,
        });
        setScrollPolicy(this._scroll);
        this._scroll.child = this._list;

        // Paging happens on scroll rather than behind a button, so a long
        // archive reads as one list however much of it has been fetched.
        this._adjustment = verticalAdjustment(this._scroll);
        this._adjustment?.connect('notify::value', () => this._maybeLoadMore());

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

        this._clearButton = new St.Button({
            styleClass: 'rldyour-clear',
            label: 'Clear',
            canFocus: true,
        });
        this._clearButton.connect('clicked', () => this._onClear());
        box.add_child(this._clearButton);

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
        this._updateSummary().catch(error => this._report(error));
    }

    async _load() {
        if (this._loading || this._exhausted)
            return;
        this._loading = true;

        try {
            const items = await this._client.list({
                limit: this._tab.pageSize,
                before: this._oldest,
                query: this._query || null,
                kind: this._kind,
                pinned: this._tab.pinned,
            });

            // A short page is the end of the archive, so no further request is
            // made however far the list is scrolled.
            if (items.length < this._tab.pageSize)
                this._exhausted = true;
            for (const entry of items)
                this._addRow(entry);
            if (items.length > 0)
                this._oldest = items[items.length - 1].id;

            this._showPlaceholder();
        } finally {
            this._loading = false;
        }
    }

    _maybeLoadMore() {
        const adjustment = this._adjustment;
        if (!adjustment || this._loading || this._exhausted)
            return;
        const remaining = adjustment.upper - adjustment.pageSize - adjustment.value;
        if (remaining < PREFETCH_MARGIN)
            this._load().catch(error => this._report(error));
    }

    _showPlaceholder() {
        const empty = this._rows.size === 0;
        this._empty.visible = empty;
        this._scroll.visible = !empty;
        this._empty.text = this._query
            ? 'Nothing matches that.'
            : this._tab.pinned
                ? 'No favorites yet. Star an entry to keep it here.'
                : 'Nothing here yet. Copy something.';
    }

    // -- rows ------------------------------------------------------------

    /**
     * Builds a row and connects it.
     *
     * One place, because a row created on an archive broadcast must behave
     * exactly like one created by a list request — and two copies of the
     * wiring is how that stops being true.
     */
    _makeRow(entry) {
        const row = new Row(entry, this._client);
        row.connect('activated', (_row, paste) => this.emit('activated', row.entry, paste));
        row.connect('pin-toggled', () => this._onPin(row.entry));
        row.connect('removed', () => this._onRemove(row.entry));
        this._rows.set(entry.id, row);
        return row;
    }

    _addRow(entry) {
        this._list.add_child(this._makeRow(entry));
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
            // Only at the unfiltered top of a list the entry belongs on:
            // inserting into a search an entry may not match would be a lie
            // about the search, and a fresh copy is never a favorite.
            if (!this._query && !this._kind
                && event.entry.pinned === this._tab.pinned
                && !this._rows.has(event.entry.id)) {
                this._list.insert_child_at_index(this._makeRow(event.entry), 0);
                this._showPlaceholder();
            }
            break;
        case 'updated':
            // Both a pin toggle and a re-copy land here; either can move the
            // entry between the tabs or within the order. Rebuilding is the
            // only update that is right for every one of those.
            if (this._rows.has(event.entry.id)
                || event.entry.pinned === this._tab.pinned)
                this.refresh();
            break;
        case 'removed':
            this._rows.get(event.entry)?.destroy();
            this._rows.delete(event.entry);
            this._showPlaceholder();
            break;
        case 'cleared':
            this.refresh();
            break;
        }
        this._updateSummary().catch(error => this._report(error));
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
            const rows = this._list.get_children();
            const row = rows[this._selected >= 0 ? this._selected : 0];
            if (row) {
                // Shift puts the entry on the clipboard without typing the
                // paste, for somewhere the shortcut would be wrong.
                const paste = (event.get_state() & Clutter.ModifierType.SHIFT_MASK) === 0;
                this.emit('activated', row.entry, paste);
            }
            return Clutter.EVENT_STOP;
        }

        default:
            return Clutter.EVENT_PROPAGATE;
        }
    }

    _move(delta) {
        // The list's children are the visual order; _rows is a lookup by id
        // and its insertion order can lag behind inserted rows.
        const rows = this._list.get_children();
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

    _onTabClicked(tab) {
        if (tab === this._tab)
            return;
        this._tab = tab;
        for (const {button, tab: own} of this._tabButtons)
            button.checked = own === tab;
        // Clearing history makes no sense from the page history cannot reach.
        this._clearButton.visible = !tab.pinned;
        this.refresh();
    }

    _onFilterClicked(kind) {
        this._kind = kind;
        for (const {button, kind: own} of this._filterButtons)
            button.checked = own === kind;
        this.refresh();
    }

    _onPin(entry) {
        this._client.pin(entry.id, !entry.pinned)
            // The daemon's `updated` broadcast rebuilds the list, moving the
            // entry to the tab it now belongs on.
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
            `${stats.pinned} ${stats.pinned === 1 ? 'favorite' : 'favorites'} · ` +
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
        // Every open lands on the live stream; the store is one tap away.
        if (this._tab !== TABS[0])
            this._onTabClicked(TABS[0]);
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
 * One archived entry.
 *
 * A row shows what the summary already says. The one thing it fetches is a
 * thumbnail, and only for an entry the daemon has already made one for — so
 * drawing the list never decodes an image in this process.
 */
const Row = GObject.registerClass({
    GTypeName: 'RldyourClipboardRow',
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

    /** The summary this row was built from, kept current by the picker. */
    get entry() {
        return this._entry;
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

        // These sit inside the row, which is itself a button. A nested
        // St.Button consumes the press that reaches it, so clicking one does
        // not also activate the row underneath.
        this._pin = new St.Button({
            styleClass: entry.pinned ? 'rldyour-action rldyour-pinned' : 'rldyour-action',
            canFocus: true,
            child: new St.Icon({
                iconName: entry.pinned ? 'starred-symbolic' : 'non-starred-symbolic',
                styleClass: 'rldyour-action-icon',
            }),
        });
        this._pin.connect('clicked', () => this.emit('pin-toggled'));
        box.add_child(this._pin);

        const remove = new St.Button({
            styleClass: 'rldyour-action',
            canFocus: true,
            child: new St.Icon({
                iconName: 'user-trash-symbolic',
                styleClass: 'rldyour-action-icon',
            }),
        });
        remove.connect('clicked', () => this.emit('removed'));
        box.add_child(remove);

        return box;
    }

    /**
     * Replaces the kind icon with the daemon's thumbnail.
     *
     * The daemon decoded them, so nothing is decoded here. A list of forty
     * rows would otherwise be forty decodes on the compositor's thread, which
     * is the one thing this whole split exists to avoid. `setImageBytes`
     * covers the argument change GNOME 48 made to `set_bytes`.
     */
    _loadThumbnail() {
        this._client.thumb(this._entry.id)
            .then(({width, height, stride, bytes}) => {
                if (this._cancellable.is_cancelled())
                    return;
                const content = St.ImageContent.new_with_preferred_size(width, height);
                setImageBytes(content, bytes, width, height, stride);
                this._glyph.styleClass = 'rldyour-thumb';
                this._glyph.gicon = content;
            })
            .catch(error => {
                // An entry removed between the list and the thumbnail, or one
                // whose thumbnail will not decode. The kind icon is already
                // showing, so there is nothing to put right.
                console.debug(`rldyour-clipboard: no thumbnail for ${this._entry.id}: ${error}`);
            });
    }

    setHighlighted(on) {
        if (on)
            this.add_style_class_name('rldyour-row-selected');
        else
            this.remove_style_class_name('rldyour-row-selected');
    }
});
