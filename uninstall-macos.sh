#!/usr/bin/env bash
# Removes the program, never the clipboard archive.
set -euo pipefail
for label in io.nddev.rldyour-clipboard io.nddev.rldyour-clipboardd; do
  launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
  target="${HOME}/Library/LaunchAgents/${label}.plist"
  if [[ -f "${target}" && ! -L "${target}" ]]; then rm "${target}"; fi
done
APP_DIR="${HOME}/Applications/rldyour-clipboard.app"
if [[ -d "${APP_DIR}" && ! -L "${APP_DIR}" ]]; then
  app_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "${APP_DIR}/Contents/Info.plist")"
  if [[ "${app_id}" == 'io.nddev.rldyour-clipboard' ]]; then rm -r "${APP_DIR}"; fi
fi
BIN="${HOME}/.local/bin/rldyour-clipboardd"
if [[ -f "${BIN}" && ! -L "${BIN}" ]]; then rm "${BIN}"; fi
printf 'Removed code and login agents. Clipboard archive preserved.\n'
