#!/usr/bin/env bash
# rldyour-clipboard installer
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds the daemon, installs it as a socket-activated user service, and puts
# the extension where the shell looks for it. Everything lands under $HOME; no
# step needs root.
set -euo pipefail

UUID="rldyour-clipboard@nddev-opennetwork"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"
EXT_DIR="${HOME}/.local/share/gnome-shell/extensions/${UUID}"

say() { printf '\033[1m==>\033[0m %s\n' "$1"; }

say "Building the daemon"
cargo build --release --manifest-path "${ROOT}/daemon/Cargo.toml"

say "Installing the daemon into ${BIN_DIR}"
install -Dm755 "${ROOT}/daemon/target/release/rldyour-clipboardd" \
  "${BIN_DIR}/rldyour-clipboardd"

say "Installing the user units into ${UNIT_DIR}"
install -Dm644 "${ROOT}/daemon/systemd/rldyour-clipboardd.socket" \
  "${UNIT_DIR}/rldyour-clipboardd.socket"
install -Dm644 "${ROOT}/daemon/systemd/rldyour-clipboardd.service" \
  "${UNIT_DIR}/rldyour-clipboardd.service"

# The unit lists this directory as writable, so it has to exist before the
# service is allowed to start.
mkdir -p "${HOME}/.local/share/rldyour-clipboard"

say "Enabling the socket"
systemctl --user daemon-reload
# The socket carries the activation: the service starts on the first
# connection and exits again once nobody has been connected for a while.
systemctl --user enable --now rldyour-clipboardd.socket

say "Installing the extension into ${EXT_DIR}"
rm -rf "${EXT_DIR}"
mkdir -p "${EXT_DIR}"
cp -r "${ROOT}/extension/." "${EXT_DIR}/"
# The extension payload matches the released zip: tests stay in the repo.
rm -rf "${EXT_DIR}/tests"
# The shell reads the compiled binary form, never the XML source.
glib-compile-schemas "${EXT_DIR}/schemas"

say "Done"
if [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  cat <<'NOTE'

The daemon is live. Under Wayland the shell cannot be restarted in place, so
log out and back in, then run:

    gnome-extensions enable rldyour-clipboard@nddev-opennetwork

NOTE
else
  cat <<'NOTE'

The daemon is live. On X11 the shell can reload in place: press Alt+F2, type
r and press Enter. Then run:

    gnome-extensions enable rldyour-clipboard@nddev-opennetwork

NOTE
fi
