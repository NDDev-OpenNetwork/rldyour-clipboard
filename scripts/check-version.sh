#!/usr/bin/env bash
# Checks that every place carrying a version agrees.
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The daemon, the Python client and the changelog are released together, so a
# mismatch is a release that ships two different answers to "which version is
# this".
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0

crate="$(grep -m1 '^version = ' "${ROOT}/daemon/Cargo.toml" | cut -d'"' -f2)"
python_project="$(grep -m1 '^version = ' "${ROOT}/python/pyproject.toml" | cut -d'"' -f2)"
python_module="$(grep -m1 '^__version__ = ' "${ROOT}/python/src/rldyour_clipboard/__init__.py" | cut -d'"' -f2)"

report() { printf '  %s %s\n' "$1" "$2"; }

if [ "${crate}" = "${python_project}" ] && [ "${crate}" = "${python_module}" ]; then
  report "ok  " "every version is ${crate}"
else
  report "FAIL" "daemon ${crate}, pyproject ${python_project}, module ${python_module}"
  FAILED=1
fi

if grep -q "^## ${crate}" "${ROOT}/CHANGELOG.md"; then
  report "ok  " "the changelog has an entry for ${crate}"
else
  report "FAIL" "the changelog has no entry for ${crate}"
  FAILED=1
fi

# The lock file has to carry the crate's own version too, or a --locked build
# in CI fails after a version bump that forgot it.
if grep -A1 '^name = "rldyour-clipboardd"' "${ROOT}/daemon/Cargo.lock" | grep -q "\"${crate}\""; then
  report "ok  " "Cargo.lock agrees"
else
  report "FAIL" "Cargo.lock does not carry ${crate}"
  FAILED=1
fi

exit "${FAILED}"
