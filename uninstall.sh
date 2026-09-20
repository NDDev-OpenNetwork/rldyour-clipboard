#!/usr/bin/env bash
# rldyour-clipboard uninstaller
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Removes the program and leaves the archive alone: what the user copied is
# their data, and deleting it is a separate, deliberate act.
set -euo pipefail

UUID="rldyour-clipboard@nddev-opennetwork"
ARCHIVE="${XDG_DATA_HOME:-${HOME}/.local/share}/rldyour-clipboard"

systemctl --user disable --now rldyour-clipboardd.socket 2>/dev/null || true
systemctl --user stop rldyour-clipboardd.service 2>/dev/null || true
rm -f "${HOME}/.config/systemd/user/rldyour-clipboardd.socket"
rm -f "${HOME}/.config/systemd/user/rldyour-clipboardd.service"
systemctl --user daemon-reload

rm -f "${HOME}/.local/bin/rldyour-clipboardd"
rm -rf "${HOME}/.local/share/gnome-shell/extensions/${UUID}"

printf 'Removed. The tray icon disappears at the next login.\n'
if [ -d "${ARCHIVE}" ]; then
  printf 'The archive is still at %s; delete it yourself if you want it gone.\n' "${ARCHIVE}"
fi
