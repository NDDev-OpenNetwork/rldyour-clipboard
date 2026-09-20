/* rldyour-clipboard — checks for the extension's pure logic
 * Copyright (C) 2026 NDDev OpenNetwork
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Run with `gjs -m extension/tests/smoke.js`.
 *
 * The modules that draw or touch the selection import St, Clutter and Meta,
 * which exist only inside the shell process and cannot be loaded here. That is
 * exactly why the decisions worth testing — what to archive, what to serve
 * back, how to label a row — were kept in modules that import none of them.
 */

import {age, iconFor, size, swatch, title} from '../lib/format.js';
import {
    MAX_REPRESENTATIONS,
    anySensitive,
    preferredMime,
    rank,
    recordable,
} from '../lib/mimes.js';

let failures = 0;

function check(condition, what) {
    print(`  ${condition ? '\x1b[32mok\x1b[0m  ' : '\x1b[31mFAIL\x1b[0m'} ${what}`);
    if (!condition)
        failures += 1;
}

function equal(actual, expected, what) {
    check(actual === expected, `${what}${actual === expected ? '' : ` (got ${JSON.stringify(actual)}, wanted ${JSON.stringify(expected)})`}`);
}

print('Row titles');
equal(title({kind: 'text', preview: 'git rebase'}), 'git rebase',
    'a preview is the title');
equal(title({kind: 'image', preview: null, width: 1920, height: 1080}),
    'Image, 1920×1080', 'an image without text is described by its size');
equal(title({kind: 'files', preview: null}), 'Files',
    'an entry with nothing to show falls back to its kind');

print('Ages');
equal(age(1000, 1000), 'now', 'the present is "now"');
equal(age(1000, 1030), 'now', 'under a minute is still "now"');
equal(age(1000, 1000 + 120), '2m', 'minutes');
equal(age(1000, 1000 + 7200), '2h', 'hours');
equal(age(1000, 1000 + 86400 * 3), '3d', 'days');
equal(age(1000, 1000 + 86400 * 21), '3w', 'weeks');
// A clock that went backwards must not print a negative age.
equal(age(2000, 1000), 'now', 'a timestamp in the future reads as now');

print('Sizes');
equal(size(512), '512 B', 'bytes');
equal(size(2048), '2.0 kB', 'one decimal below ten');
equal(size(1024 * 1024 * 340), '340 MB', 'no decimal above ten');
equal(size(1024 * 1024 * 1024 * 3), '3.0 GB', 'gigabytes');

print('Colour swatches');
equal(swatch('#abc'), '#aabbcc', 'the short form expands');
equal(swatch('#AABBCC'), '#AABBCC', 'the long form is kept');
equal(swatch('#aabbccdd'), '#aabbcc', 'alpha is dropped for the swatch');
equal(swatch('rgb(1, 2, 3)'), 'rgb(1, 2, 3)', 'a function is passed through');
equal(swatch('not a colour'), null, 'prose is not a colour');
equal(swatch(null), null, 'nothing is not a colour');

print('Kind icons');
check(iconFor('image') !== iconFor('text'), 'each kind has its own icon');
equal(iconFor('nonesuch'), iconFor('text'), 'an unknown kind falls back');

print('Secrets');
check(anySensitive(['text/plain', 'x-kde-passwordManagerHint']),
    'a password hint condemns the whole event');
check(anySensitive(['TEXT/PLAIN', 'X-KDE-PasswordManagerHint']),
    'the hint is recognised whatever its case');
check(!anySensitive(['text/plain', 'text/html']),
    'ordinary text is not a secret');

print('What gets archived');
const offered = [
    'TARGETS', 'TIMESTAMP', 'SAVE_TARGETS', 'MULTIPLE',
    'text/plain', 'text/html', 'UTF8_STRING', 'image/png',
];
const wanted = recordable(offered);
check(!wanted.includes('TARGETS'), 'protocol targets are not content');
check(!wanted.includes('TIMESTAMP'), 'a timestamp target is not content');
check(wanted.includes('UTF8_STRING'), 'a known X11 text atom is content');
equal(wanted[0], 'image/png', 'the richest representation sorts first');
check(wanted.indexOf('text/html') < wanted.indexOf('text/plain'),
    'rich text sorts before flattened text');

equal(recordable(['text/plain', 'TEXT/PLAIN', 'text/plain']).length, 1,
    'a type offered twice is recorded once');
equal(recordable([]).length, 0, 'nothing offered is nothing recorded');
check(recordable(Array.from({length: 40}, (_, i) => `image/x-${i}`)).length ===
    MAX_REPRESENTATIONS, 'the number of representations is capped');
check(rank('image/png') < rank('text/plain'), 'an image outranks text');
check(rank('application/x-unknown') > rank('text/plain'),
    'an unknown type ranks last');

print('What gets pasted back');
equal(preferredMime({kind: 'image', mimes: ['image/png', 'text/html']}), 'image/png',
    'an image restores as itself');
equal(preferredMime({kind: 'files', mimes: ['x-special/gnome-copied-files']}),
    'x-special/gnome-copied-files', 'a file reference restores as itself');
// The important one: HTML offered to a terminal matches nothing and pastes
// nothing, whereas plain text into a rich editor merely loses its styling.
equal(preferredMime({kind: 'text', mimes: ['text/html', 'text/plain']}), 'text/plain',
    'text restores as plain text even when HTML is held');
equal(preferredMime({kind: 'text', mimes: ['text/html']}), 'text/html',
    'with no plain text, the only representation is used');
equal(preferredMime({kind: 'text', mimes: []}), null, 'an entry with nothing has nothing');

print('');
if (failures > 0) {
    print(`\x1b[31m${failures} check(s) failed\x1b[0m`);
    imports.system.exit(1);
}
print('\x1b[32mAll checks passed\x1b[0m');
