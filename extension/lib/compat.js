/* rldyour-clipboard — differences between the shell versions supported
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * This extension declares GNOME Shell 46 through 50, and three of the APIs it
 * needs changed inside that range. Each difference is handled here, once,
 * against the shell's own reported version — the same thing the shell uses to
 * decide whether an extension is compatible at all.
 *
 * Version detection rather than feature detection, deliberately: `'x' in obj`
 * cannot tell an absent GObject property from one GJS is willing to set as a
 * plain JavaScript field, and a try/catch around an arity change turns a
 * miscount into silence.
 */

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import St from 'gi://St';

import * as Config from 'resource:///org/gnome/shell/misc/config.js';

const [SHELL_MAJOR] = Config.PACKAGE_VERSION.split('.').map(Number);

/**
 * A vertical box, however this shell spells it.
 *
 * `St.BoxLayout` carried a `vertical` boolean from GNOME 45. GNOME 47 added
 * `orientation`, GNOME 48 deprecated `vertical`, and GNOME 51 removes it.
 *
 * Both halves matter. On GNOME 46 `orientation` does not exist, and an unknown
 * property in a GObject constructor throws — which stops the whole extension
 * loading rather than misplacing one box. On GNOME 48 and later `vertical`
 * still works but logs a deprecation for every box built.
 */
export function verticalBox(properties = {}) {
    const box = new St.BoxLayout(properties);
    setVertical(box);
    return box;
}

export function setVertical(box) {
    if (SHELL_MAJOR >= 47)
        box.orientation = Clutter.Orientation.VERTICAL;
    else
        box.vertical = true;
}

/**
 * Uploads raw pixels into an `St.ImageContent`.
 *
 * GNOME 48 gave `set_bytes` a new first parameter, a `Cogl.Context`. Calling
 * it with the old argument count throws, so this is not something to get
 * wrong quietly.
 *
 * `St.ImageContent` is the only GIcon the shell's texture cache renders from
 * raw data: it tests for that type specifically and otherwise falls through to
 * an icon theme lookup, which finds nothing for, say, a `Gio.BytesIcon`.
 *
 * @param {St.ImageContent} content the content to fill
 * @param {GLib.Bytes} bytes straight RGBA, `height` rows of `stride`
 * @param {number} width pixels
 * @param {number} height pixels
 * @param {number} stride bytes per row
 */
export function setImageBytes(content, bytes, width, height, stride) {
    const format = Cogl.PixelFormat.RGBA_8888;

    if (SHELL_MAJOR >= 48) {
        const cogl = global.stage.context.get_backend().get_cogl_context();
        content.set_bytes(cogl, bytes, format, width, height, stride);
    } else {
        content.set_bytes(bytes, format, width, height, stride);
    }
}

/**
 * Sets the scrollbar policy.
 *
 * `st_scroll_view_set_policy` predates GNOME 46 and is still on main, so this
 * needs no branch — it is a named function purely so the call site reads as
 * intent rather than as two enum values.
 */
export function setScrollPolicy(scrollView) {
    scrollView.set_policy(St.PolicyType.NEVER, St.PolicyType.AUTOMATIC);
}

/**
 * The vertical adjustment of a scroll view.
 *
 * `st_scroll_view_get_vadjustment` exists throughout the supported range.
 */
export function verticalAdjustment(scrollView) {
    return scrollView.vadjustment ?? null;
}
