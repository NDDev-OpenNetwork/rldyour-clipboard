#!/usr/bin/env bash
# Checks the facts duplicated between the daemon and the extension.
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Some tables genuinely have to exist twice: the extension decides what to
# capture and the daemon decides what to serve back, and they are written in
# different languages in different processes. Nothing stops them drifting
# apart except this.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0

fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; FAILED=1; }
pass() { printf '  \033[32mok\033[0m   %s\n' "$1"; }

extract() {
  python3 - "$1" "$2" <<'PY_EXTRACT'
import pathlib,re,sys
m=re.search(sys.argv[2],pathlib.Path(sys.argv[1]).read_text(),re.M)
if m is None: raise SystemExit("missing invariant: "+sys.argv[2])
print(m.group(1))
PY_EXTRACT
}

echo "Mime preference tables"
python3 - "${ROOT}" <<'PY' || FAILED=1
import pathlib, re, sys
root = pathlib.Path(sys.argv[1])

rust = (root / "daemon/src/kind.rs").read_text()
# `"image/png" => 0,` and `"image/jpeg" | "image/jpg" => 2,`
rust_table = {}
for names, value in re.findall(r'^\s*((?:"[^"]+"\s*\|?\s*)+)=>\s*(\d+),', rust, re.M):
    for name in re.findall(r'"([^"]+)"', names):
        rust_table[name] = int(value)

js = (root / "extension/lib/mimes.js").read_text()
js_table = {
    name: int(value)
    for name, value in re.findall(r"^\s*'([^']+)':\s*(\d+),", js, re.M)
}

only_rust = sorted(set(rust_table) - set(js_table))
only_js = sorted(set(js_table) - set(rust_table))
differ = sorted(m for m in set(rust_table) & set(js_table) if rust_table[m] != js_table[m])

for mime in only_rust:
    print(f"  \033[31mFAIL\033[0m {mime!r} is ranked in the daemon but not in the extension")
for mime in only_js:
    print(f"  \033[31mFAIL\033[0m {mime!r} is ranked in the extension but not in the daemon")
for mime in differ:
    print(f"  \033[31mFAIL\033[0m {mime!r} ranks {rust_table[mime]} in the daemon, {js_table[mime]} in the extension")

if not (only_rust or only_js or differ):
    print(f"  \033[32mok\033[0m   {len(rust_table)} mime ranks agree")
sys.exit(1 if (only_rust or only_js or differ) else 0)
PY

echo "Password-manager hints"
python3 - "${ROOT}" <<'PY' || FAILED=1
import pathlib, re, sys
root = pathlib.Path(sys.argv[1])

def block(text, marker):
    start = text.index(marker)
    end = text.index("]", start)
    return {name.lower() for name in re.findall(r'"([^"]+)"|\'([^\']+)\'', text[start:end]) for name in name if name}

rust = (root / "daemon/src/kind.rs").read_text()
rust_hints = {m.lower() for m in re.findall(r'"([^"]+)"', rust[rust.index("const SENSITIVE"):rust.index("];", rust.index("const SENSITIVE"))])}

js = (root / "extension/lib/mimes.js").read_text()
js_hints = {m.lower() for m in re.findall(r"'([^']+)'", js[js.index("const SENSITIVE"):js.index("];", js.index("const SENSITIVE"))])}

if rust_hints == js_hints:
    print(f"  \033[32mok\033[0m   {len(rust_hints)} secret hints agree")
    sys.exit(0)

for hint in sorted(rust_hints - js_hints):
    print(f"  \033[31mFAIL\033[0m {hint!r} is a secret to the daemon but not to the extension")
for hint in sorted(js_hints - rust_hints):
    print(f"  \033[31mFAIL\033[0m {hint!r} is a secret to the extension but not to the daemon")
sys.exit(1)
PY

echo "Selection-filter rules"
python3 - "${ROOT}" <<'PY' || FAILED=1
import pathlib, re, sys
root = pathlib.Path(sys.argv[1])

rust = (root / "daemon/src/kind.rs").read_text()
js = (root / "extension/lib/mimes.js").read_text()

def block(text, marker, quote):
    start = text.index(marker)
    # `];` terminates the table in both languages — a plain "]" would stop at
    # the Rust `&[&str]` type annotation instead.
    end = text.index("];", start)
    return {m.lower() for m in re.findall(quote + r"([^" + quote + r"]+)" + quote, text[start:end])}

rust_targets = block(rust, "const PROTOCOL_TARGETS", '"')
js_targets = block(js, "const PROTOCOL_TARGETS", "'")

ok = True
for name in sorted(rust_targets - js_targets):
    print(f"  \033[31mFAIL\033[0m {name!r} is protocol noise to the daemon but not to the extension")
    ok = False
for name in sorted(js_targets - rust_targets):
    print(f"  \033[31mFAIL\033[0m {name!r} is protocol noise to the extension but not to the daemon")
    ok = False

rust_cap = int(re.search(r'MAX_REPRESENTATIONS: usize = (\d+)', rust).group(1))
js_cap = int(re.search(r'MAX_REPRESENTATIONS = (\d+)', js).group(1))
if rust_cap != js_cap:
    print(f"  \033[31mFAIL\033[0m representation cap is {rust_cap} in the daemon, {js_cap} in the extension")
    ok = False

# A bare atom survives recordable() only by being ranked: the daemon's
# RANKED_BARE must be exactly the ranked names that carry no slash.
rust_bare = block(rust, "const RANKED_BARE", '"')
js_bare = {name for name in re.findall(r"^\s*'([^']+)':\s*\d+,", js, re.M) if '/' not in name}
if rust_bare != js_bare:
    print(f"  \033[31mFAIL\033[0m ranked bare atoms differ: daemon {sorted(rust_bare)}, extension {sorted(js_bare)}")
    ok = False

if ok:
    print(f"  \033[32mok\033[0m   {len(rust_targets)} protocol targets, {len(rust_bare)} bare atoms and the cap of {rust_cap} agree")
sys.exit(0 if ok else 1)
PY

echo "Protocol version"
rust_version="$(extract "${ROOT}/daemon/src/proto.rs" 'PROTOCOL_VERSION: u32 = (\d+)')"
js_version="$(extract "${ROOT}/extension/lib/client.js" 'const PROTOCOL_VERSION = (\d+)')"
py_version="$(extract "${ROOT}/python/src/rldyour_clipboard/__init__.py" '^PROTOCOL_VERSION = (\d+)')"
if [ "${rust_version}" = "${js_version}" ] && [ "${rust_version}" = "${py_version}" ]; then
  pass "every client speaks version ${rust_version}"
else
  fail "daemon ${rust_version}, extension ${js_version}, python ${py_version}"
fi

echo "Socket name"
rust_socket="$(extract "${ROOT}/daemon/src/net.rs" 'SOCKET_NAME: &str = "([^"]+)"')"
js_socket="$(extract "${ROOT}/extension/lib/client.js" 'const SOCKET_NAME = '\''([^'\'']+)'\''')"
py_socket="$(extract "${ROOT}/python/src/rldyour_clipboard/__init__.py" '(rldyour-clipboard\.sock)')"
unit_socket="$(extract "${ROOT}/daemon/systemd/rldyour-clipboardd.socket" '^ListenStream=%t/(.*)')"
if [ "${rust_socket}" = "${js_socket}" ] && [ "${rust_socket}" = "${py_socket}" ] \
   && [ "${rust_socket}" = "${unit_socket}" ]; then
  pass "every client and the systemd unit use ${rust_socket}"
else
  fail "daemon ${rust_socket}, extension ${js_socket}, python ${py_socket}, unit ${unit_socket}"
fi

echo "Frame size limit"
rust_frame="$(extract "${ROOT}/daemon/src/proto.rs" 'MAX_FRAME: usize = ([^;]+)' | tr -d ' ')"
js_frame="$(extract "${ROOT}/extension/lib/framing.js" 'const MAX_FRAME = ([^;]+)' | tr -d ' ')"
py_frame="$(extract "${ROOT}/python/src/rldyour_clipboard/__init__.py" '^MAX_FRAME = ([^\n]+)' | tr -d ' ')"
if [ "${rust_frame}" = "${js_frame}" ] && [ "${rust_frame}" = "${py_frame}" ]; then
  pass "every side caps a control frame at ${rust_frame}"
else
  fail "daemon ${rust_frame}, extension ${js_frame}, python ${py_frame}"
fi

echo "launchd agent"
# The daemon asks launchd for a socket by name; the plist must offer that
# same key and land it on the socket file every client opens.
plist="${ROOT}/daemon/launchd/io.nddev.rldyour-clipboardd.plist"
socket_key="$(extract "${ROOT}/daemon/src/net.rs" 'launch_activate_socket\(c"([^"]+)"')"
if [ -f "${plist}" ] \
    && grep -q "<key>${socket_key}</key>" "${plist}" \
    && grep -q "/${rust_socket}</string>" "${plist}"; then
  pass "the launchd agent serves '${socket_key}' at ${rust_socket}"
else
  fail "the launchd agent does not match the daemon's launch_activate_socket contract"
fi

echo "Archive directory"
# The unit grants write access to exactly one path; the daemon must agree.
unit_path="$(extract "${ROOT}/daemon/systemd/rldyour-clipboardd.service" '^ReadWritePaths=%h/(.*)')"
if grep -q "\.local/share/${unit_path##*/}" "${ROOT}/daemon/src/store/mod.rs"; then
  pass "the unit grants write access to the archive the daemon opens"
else
  fail "the unit writes %h/${unit_path} but the daemon opens somewhere else"
fi

echo "Favorites filter"
# `pinned` is spelled out in three languages; a rename on one side silently
# turns the favorites page into an unfiltered list on the others.
rust_pinned="$(grep -c 'pinned' "${ROOT}/daemon/src/proto.rs")"
js_pinned="$(grep -c 'pinned' "${ROOT}/extension/lib/client.js")"
py_pinned="$(grep -c 'pinned' "${ROOT}/python/src/rldyour_clipboard/__init__.py")"
if [ "${rust_pinned}" -gt 0 ] && [ "${js_pinned}" -gt 0 ] && [ "${py_pinned}" -gt 0 ]; then
  pass "list's pinned filter exists in the daemon and both clients"
else
  fail "pinned wiring is partial: daemon ${rust_pinned}, extension ${js_pinned}, python ${py_pinned}"
fi

exit "${FAILED}"
