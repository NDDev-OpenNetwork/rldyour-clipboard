/* rldyour-clipboard — presentation helpers
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

/** What a row shows when an entry has no text worth previewing. */
const UNTITLED = {
    text: 'Text',
    link: 'Link',
    color: 'Colour',
    image: 'Image',
    files: 'Files',
};

/** Symbolic icon per entry kind, all from the standard icon theme. */
const ICONS = {
    text: 'format-text-rich-symbolic',
    link: 'insert-link-symbolic',
    color: 'color-select-symbolic',
    image: 'image-x-generic-symbolic',
    files: 'folder-symbolic',
};

export function iconFor(kind) {
    return ICONS[kind] ?? ICONS.text;
}

/**
 * A short label for a list row.
 *
 * The daemon has already collapsed the text to one line and truncated it, so
 * this only has to cover the entries that carry no text at all.
 */
export function title(entry) {
    if (entry.preview)
        return entry.preview;
    if (entry.kind === 'image' && entry.width && entry.height)
        return `Image, ${entry.width}×${entry.height}`;
    return UNTITLED[entry.kind] ?? UNTITLED.text;
}

/**
 * How long ago, in the shortest form that is still unambiguous.
 *
 * Deliberately coarse: the point of the column is to separate "just now" from
 * "last week", and a row is not the place for a timestamp.
 */
export function age(seconds, now = Math.floor(Date.now() / 1000)) {
    const elapsed = Math.max(0, now - seconds);

    if (elapsed < 60)
        return 'now';
    if (elapsed < 3600)
        return `${Math.floor(elapsed / 60)}m`;
    if (elapsed < 86400)
        return `${Math.floor(elapsed / 3600)}h`;
    if (elapsed < 86400 * 7)
        return `${Math.floor(elapsed / 86400)}d`;
    return `${Math.floor(elapsed / (86400 * 7))}w`;
}

/** A size a person can read, in the units a file manager would use. */
export function size(bytes) {
    if (bytes < 1024)
        return `${bytes} B`;

    const units = ['kB', 'MB', 'GB', 'TB'];
    let value = bytes / 1024;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
        value /= 1024;
        unit += 1;
    }
    // One decimal below ten, none above: "1.4 MB" but "340 MB".
    return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/**
 * The six-digit colour an entry describes, or null when it is not one.
 *
 * Only used to draw a swatch, so anything not recognised simply gets the
 * ordinary text row instead.
 */
export function swatch(preview) {
    if (!preview)
        return null;

    const text = preview.trim();
    const hex = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.exec(text);
    if (hex) {
        const digits = hex[1];
        // Expand the short forms so the caller always gets #rrggbb.
        if (digits.length <= 4)
            return `#${[...digits.slice(0, 3)].map(d => d + d).join('')}`;
        return `#${digits.slice(0, 6)}`;
    }

    return /^(rgb|hsl)a?\(/i.test(text) ? text : null;
}
