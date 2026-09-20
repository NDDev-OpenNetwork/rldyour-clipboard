#!/usr/bin/env bash
# Static checks for the GNOME Shell extension.
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The same script CI runs. Nothing here needs a running shell, which matters
# because under Wayland there is no way to reload extension code without a new
# login — so a mistake caught here costs seconds and one caught at runtime
# costs a session.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="${ROOT}/extension"
FAILED=0

fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; FAILED=1; }
pass() { printf '  \033[32mok\033[0m   %s\n' "$1"; }

echo "Syntax"
while IFS= read -r file; do
  if node --check "${file}" 2>/dev/null; then pass "${file#"${EXT}/"}"
  else fail "${file#"${EXT}/"} does not parse"; fi
done < <(find "${EXT}" -name '*.js' | sort)

echo "Metadata"
META="${EXT}/metadata.json"
if python3 -c "import json,sys; json.load(open('${META}'))" 2>/dev/null; then
  pass "metadata.json is valid JSON"
else
  fail "metadata.json is not valid JSON"
fi
python3 - "${META}" "${EXT}" <<'PY' || FAILED=1
import json, pathlib, re, sys
meta = json.loads(pathlib.Path(sys.argv[1]).read_text())
ext = pathlib.Path(sys.argv[2])
bad = False

for field in ("uuid", "name", "description", "shell-version", "url"):
    if not meta.get(field):
        print(f"  \033[31mFAIL\033[0m metadata.json is missing {field}"); bad = True

uuid = meta.get("uuid", "")
if not re.fullmatch(r"[A-Za-z0-9._-]+@[A-Za-z0-9._-]+", uuid):
    print(f"  \033[31mFAIL\033[0m uuid {uuid!r} is not id@namespace"); bad = True
elif uuid.endswith("gnome.org"):
    print("  \033[31mFAIL\033[0m uuid may not use the gnome.org namespace"); bad = True
else:
    print(f"  \033[32mok\033[0m   uuid {uuid}")

# A declared schema that does not exist fails only at runtime, in a process
# that cannot easily be restarted, so it is worth catching here.
schema = meta.get("settings-schema")
if schema:
    sources = list((ext / "schemas").glob("*.gschema.xml"))
    ids = {m for s in sources for m in re.findall(r'schema id="([^"]+)"', s.read_text())}
    if schema in ids:
        print(f"  \033[32mok\033[0m   settings-schema {schema} is defined")
    else:
        print(f"  \033[31mFAIL\033[0m settings-schema {schema} is declared but not defined"); bad = True

sys.exit(1 if bad else 0)
PY

echo "Process isolation"
# Shell-process code may not touch the toolkit libraries, and preferences code
# may not touch the shell's own. Either crashes the host process rather than
# failing.
if grep -rlE "gi://(Gtk|Adw|Gdk)" "${EXT}/extension.js" "${EXT}/lib" 2>/dev/null | grep -q .; then
  fail "toolkit library imported into the shell process"
else
  pass "no Gtk, Adw or Gdk in the shell process"
fi
if grep -lE "gi://(St|Clutter|Meta|Shell)" "${EXT}/prefs.js" 2>/dev/null | grep -q .; then
  fail "shell library imported into the preferences process"
else
  pass "no St, Clutter, Meta or Shell in preferences"
fi

echo "Deprecated modules"
if grep -rnE "imports\.(mainloop|lang|byteArray)|from 'gi://ByteArray'" "${EXT}" 2>/dev/null | grep -q .; then
  fail "Mainloop, Lang or ByteArray is still used"
else
  pass "no Mainloop, Lang or ByteArray"
fi

echo "Blocking calls in the shell process"
# This code runs on the compositor's thread. A synchronous read of an archive
# entry would freeze the desktop for as long as the read took, so the async
# form is not a preference here but a requirement.
if grep -rnE "\.(read_bytes|write_bytes|splice|read_line)\(" "${EXT}/lib" "${EXT}/extension.js" 2>/dev/null \
    | grep -vE "_async|//" | grep -q .; then
  grep -rnE "\.(read_bytes|write_bytes|splice|read_line)\(" "${EXT}/lib" "${EXT}/extension.js" 2>/dev/null \
    | grep -vE "_async|//" | sed 's/^/    /'
  fail "a synchronous stream call reaches the shell process"
else
  pass "stream work in the shell process is asynchronous"
fi

echo "GObject type names"
python3 "${ROOT}/scripts/check-gtypes.py" "${EXT}" || FAILED=1

echo "Version-gated APIs"
# Three APIs changed inside the declared shell range, and each one fails hard
# rather than degrading: `orientation` does not exist on 46, `vertical` is
# deprecated from 48, and `set_bytes` gained a Cogl.Context argument in 48.
# All three go through compat.js, which is the only file allowed to name them.
# Fixed-string matching, because two of these contain regex metacharacters.
gated=0
for pattern in 'orientation:' 'vertical: true' '.set_bytes('; do
  found="$(grep -rnF -- "${pattern}" "${EXT}/lib" "${EXT}/extension.js" 2>/dev/null \
    | grep -v '/compat.js:' || true)"
  if [ -n "${found}" ]; then
    printf '%s\n' "${found}" | sed 's/^/    /'
    fail "${pattern} is used outside compat.js"
    gated=1
  fi
done
if [ "${gated}" -eq 0 ]; then
  pass "orientation, vertical and set_bytes go through compat.js"
fi

echo "Settings keys"
python3 - "${EXT}" <<'PY' || FAILED=1
import pathlib, re, sys
ext = pathlib.Path(sys.argv[1])
declared = set()
for source in (ext / "schemas").glob("*.gschema.xml"):
    declared |= set(re.findall(r'<key name="([^"]+)"', source.read_text()))

used = set()
for source in ext.rglob("*.js"):
    text = source.read_text()
    used |= set(re.findall(r"get_(?:boolean|int|string|double|strv)\('([^']+)'\)", text))
    used |= set(re.findall(r"set_strv\('([^']+)'", text))
    used |= set(re.findall(r"settings\.bind\('([^']+)'", text))
    # Keybindings are named by a constant, so the constant's value counts too.
    used |= set(re.findall(r"^const SHORTCUT = '([^']+)'", text, re.MULTILINE))

missing = sorted(used - declared)
for key in missing:
    print(f"  \033[31mFAIL\033[0m setting {key!r} is read but not declared")
if not missing:
    print(f"  \033[32mok\033[0m   all {len(used)} settings read are declared")

unused = sorted(declared - used)
for key in unused:
    print(f"  \033[33mnote\033[0m declared setting {key!r} is never read")
sys.exit(1 if missing else 0)
PY

echo "Schema compiles"
if glib-compile-schemas --strict --dry-run "${EXT}/schemas" 2>/dev/null; then
  pass "gschema is valid"
else
  fail "gschema does not compile"
fi

echo "Stylesheet classes"
# A style class used by the code but absent from the stylesheet draws as an
# unstyled row, which looks like a layout bug rather than a missing rule.
python3 - "${EXT}" <<'PY'
import pathlib, re, sys
ext = pathlib.Path(sys.argv[1])
css = (ext / "stylesheet.css").read_text()
defined = set(re.findall(r"\.([a-z][a-z0-9-]*)", css))

used = set()
for source in ext.rglob("*.js"):
    text = source.read_text()
    for match in re.findall(r"styleClass: '([^']+)'", text):
        used |= {name for name in match.split() if name.startswith("rldyour-")}
    for match in re.findall(r"style_class_name\('([^']+)'\)", text):
        used |= {name for name in match.split() if name.startswith("rldyour-")}

missing = sorted(used - defined)
for name in missing:
    print(f"  \033[33mnote\033[0m style class {name!r} has no rule")
if not missing:
    print(f"  \033[32mok\033[0m   all {len(used)} style classes have rules")
PY

exit "${FAILED}"
