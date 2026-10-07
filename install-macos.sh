#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
AGENT_DIR="${HOME}/Library/LaunchAgents"
APP_DIR="${HOME}/Applications/rldyour-clipboard.app"
DATA_DIR="${HOME}/Library/Application Support/rldyour-clipboard"
if [[ -f "${ROOT}/VERSION" ]]; then
  VERSION="$(cat "${ROOT}/VERSION")"
else
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${ROOT}/daemon/Cargo.toml" | head -1)"
fi
[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 1
STAGE="$(mktemp -d)"
trap 'rm -r "${STAGE}"' EXIT
APP="${STAGE}/rldyour-clipboard.app"
install -d "${APP}/Contents/MacOS" "${BIN_DIR}" "${AGENT_DIR}" "${HOME}/Applications"
install -d -m700 "${DATA_DIR}"
if [[ -x "${ROOT}/prebuilt/rldyour-clipboardd" && -d "${ROOT}/prebuilt/rldyour-clipboard.app" ]]; then
  install -m755 "${ROOT}/prebuilt/rldyour-clipboardd" "${STAGE}/rldyour-clipboardd"
  cp -R "${ROOT}/prebuilt/rldyour-clipboard.app/." "${APP}/"
else
  cargo build --release --locked --manifest-path "${ROOT}/daemon/Cargo.toml"
  install -m755 "${ROOT}/daemon/target/release/rldyour-clipboardd" "${STAGE}/rldyour-clipboardd"
  swiftc -target "$(uname -m)-apple-macosx12.0" -swift-version 6 -parse-as-library -warnings-as-errors -O -framework AppKit "${ROOT}"/macos/*.swift -o "${APP}/Contents/MacOS/rldyour-clipboard"
fi
/usr/libexec/PlistBuddy -c 'Clear dict' "${APP}/Contents/Info.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c 'Add :CFBundleExecutable string rldyour-clipboard' "${APP}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Add :CFBundleIdentifier string io.nddev.rldyour-clipboard' "${APP}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Add :CFBundleName string rldyour-clipboard' "${APP}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string ${VERSION}" "${APP}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string ${VERSION}" "${APP}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Add :LSUIElement bool true' "${APP}/Contents/Info.plist"
codesign --force --sign - "${APP}"
if [[ -L "${APP_DIR}" ]]; then echo 'Refusing a redirected application path' >&2; exit 1; fi
if [[ -f "${APP_DIR}/Contents/Info.plist" ]]; then
  installed_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "${APP_DIR}/Contents/Info.plist")"
  [[ "${installed_id}" == 'io.nddev.rldyour-clipboard' ]] || exit 1
fi
launchctl bootout "gui/$(id -u)/io.nddev.rldyour-clipboard" 2>/dev/null || true
launchctl bootout "gui/$(id -u)/io.nddev.rldyour-clipboardd" 2>/dev/null || true
install -m755 "${STAGE}/rldyour-clipboardd" "${BIN_DIR}/rldyour-clipboardd.new"
mv -f "${BIN_DIR}/rldyour-clipboardd.new" "${BIN_DIR}/rldyour-clipboardd"
if [[ -d "${APP_DIR}" ]]; then rm -r "${APP_DIR}"; fi
mv "${APP}" "${APP_DIR}"
for source in "${ROOT}/daemon/launchd/io.nddev.rldyour-clipboardd.plist" "${ROOT}/macos/io.nddev.rldyour-clipboard.plist"; do
  target="${AGENT_DIR}/$(basename "${source}")"
  sed "s|@HOME@|${HOME}|g" "${source}" > "${target}"
  plutil -lint "${target}"
  launchctl bootstrap "gui/$(id -u)" "${target}"
done
printf 'Installed rldyour-clipboard %s. Unpinned history: 7 days; pinned history: permanent.\n' "${VERSION}"
