/* rldyour-clipboard — differences between the shell versions supported
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

import Clutter from 'gi://Clutter';
import St from 'gi://St';

/**
 * A vertical box, however this shell spells it.
 *
 * `St.BoxLayout` carried a `vertical` boolean through GNOME 46, gained
 * `orientation` alongside it in 47, and dropped `vertical` in 48. An extension
 * declaring 46 through 50 has to satisfy all three, and the property is not
 * something a static check can pick the right name for.
 */
export function verticalBox(properties = {}) {
    const box = new St.BoxLayout(properties);
    setVertical(box);
    return box;
}

export function setVertical(box) {
    if ('orientation' in box)
        box.orientation = Clutter.Orientation.VERTICAL;
    else
        box.vertical = true;
}

/**
 * Whether this shell draws scrollbars through a policy enum or the older
 * boolean pair. Only the enum form has existed since 46, so this is here to
 * keep the call site honest rather than to branch.
 */
export function setScrollPolicy(scrollView) {
    scrollView.set_policy(St.PolicyType.NEVER, St.PolicyType.AUTOMATIC);
}

/**
 * Puts a widget inside a scroll view.
 *
 * GNOME 46 takes the child through `add_actor`; 47 and later use `child`.
 */
export function setScrollChild(scrollView, child) {
    if ('child' in scrollView)
        scrollView.child = child;
    else
        scrollView.add_actor(child);
}
