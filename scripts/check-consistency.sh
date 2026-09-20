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

if ok:
    print(f"  \033[32mok\033[0m   {len(rust_targets)} protocol targets and the cap of {rust_cap} agree")
sys.exit(0 if ok else 1)
PY

echo "Protocol version"
rust_version="$(grep -oP 'PROTOCOL_VERSION: u32 = \K\d+' "${ROOT}/daemon/src/proto.rs")"
js_version="$(grep -oP 'const PROTOCOL_VERSION = \K\d+' "${ROOT}/extension/lib/client.js")"
py_version="$(grep -oP '^PROTOCOL_VERSION = \K\d+' "${ROOT}/python/src/rldyour_clipboard/__init__.py")"
if [ "${rust_version}" = "${js_version}" ] && [ "${rust_version}" = "${py_version}" ]; then
  pass "every client speaks version ${rust_version}"
else
  fail "daemon ${rust_version}, extension ${js_version}, python ${py_version}"
fi

echo "Socket name"
rust_socket="$(grep -oP 'SOCKET_NAME: &str = "\K[^"]+' "${ROOT}/daemon/src/net.rs")"
js_socket="$(grep -oP "const SOCKET_NAME = '\K[^']+" "${ROOT}/extension/lib/client.js")"
py_socket="$(grep -oP 'rldyour-clipboard\.sock' "${ROOT}/python/src/rldyour_clipboard/__init__.py" | head -1)"
unit_socket="$(grep -oP 'ListenStream=%t/\K.*' "${ROOT}/daemon/systemd/rldyour-clipboardd.socket")"
if [ "${rust_socket}" = "${js_socket}" ] && [ "${rust_socket}" = "${py_socket}" ] \
   && [ "${rust_socket}" = "${unit_socket}" ]; then
  pass "every client and the systemd unit use ${rust_socket}"
else
  fail "daemon ${rust_socket}, extension ${js_socket}, python ${py_socket}, unit ${unit_socket}"
fi

echo "Frame size limit"
rust_frame="$(grep -oP 'MAX_FRAME: usize = \K.*(?=;)' "${ROOT}/daemon/src/proto.rs" | tr -d ' ')"
js_frame="$(grep -oP 'const MAX_FRAME = \K.*(?=;)' "${ROOT}/extension/lib/client.js" | tr -d ' ')"
py_frame="$(grep -oP '^MAX_FRAME = \K.*' "${ROOT}/python/src/rldyour_clipboard/__init__.py" | tr -d ' ')"
if [ "${rust_frame}" = "${js_frame}" ] && [ "${rust_frame}" = "${py_frame}" ]; then
  pass "every side caps a control frame at ${rust_frame}"
else
  fail "daemon ${rust_frame}, extension ${js_frame}, python ${py_frame}"
fi

echo "launchd agent"
# The daemon asks launchd for a socket by name; the plist must offer that
# same key and land it on the socket file every client opens.
plist="${ROOT}/daemon/launchd/io.nddev.rldyour-clipboardd.plist"
socket_key="$(grep -oP 'CString::new\("\K[^"]+' "${ROOT}/daemon/src/main.rs" | head -1)"
if [ -f "${plist}" ] \
    && grep -q "<key>${socket_key}</key>" "${plist}" \
    && grep -q "/${rust_socket}</string>" "${plist}"; then
  pass "the launchd agent serves '${socket_key}' at ${rust_socket}"
else
  fail "the launchd agent does not match the daemon's launch_activate_socket contract"
fi

echo "Archive directory"
# The unit grants write access to exactly one path; the daemon must agree.
unit_path="$(grep -oP 'ReadWritePaths=%h/\K.*' "${ROOT}/daemon/systemd/rldyour-clipboardd.service")"
if grep -q "\.local/share/${unit_path##*/}" "${ROOT}/daemon/src/store/mod.rs"; then
  pass "the unit grants write access to the archive the daemon opens"
else
  fail "the unit writes %h/${unit_path} but the daemon opens somewhere else"
fi

exit "${FAILED}"
