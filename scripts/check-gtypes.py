#!/usr/bin/env python3
"""Every registered GObject class must name its own GType.

Left implicit, GJS derives a GType name from the file path and the class name:
a `lib/indicator.js` exporting `Indicator` becomes `Gjs_lib_indicator_Indicator`.
Any other extension laid out the same way claims exactly that name, and the
second one to load fails with "already registered" — which loses the whole
extension, not one widget. It is not a hypothetical: this repository's sibling
`rldyour-sysinfo` has the same file with the same class in it.
"""

from __future__ import annotations

import pathlib
import re
import sys

GREEN, RED, RESET = "\033[32m", "\033[31m", "\033[0m"

#: How far past `registerClass(` to look for the option.
WINDOW = 500


def main(root: pathlib.Path) -> int:
    missing: list[str] = []
    total = 0

    for source in sorted(root.rglob("*.js")):
        text = source.read_text()
        for match in re.finditer(r"GObject\.registerClass\(\s*(\{)?", text):
            total += 1
            line = text[: match.start()].count("\n") + 1
            where = f"{source.relative_to(root)}:{line}"
            if match.group(1) is None:
                # No options object at all, so nowhere to put the name.
                missing.append(f"{where} (no options object)")
            elif "GTypeName:" not in text[match.end() : match.end() + WINDOW]:
                missing.append(where)

    for where in missing:
        print(f"  {RED}FAIL{RESET} {where} registers a class without a GTypeName")
    if not missing:
        print(f"  {GREEN}ok{RESET}   all {total} registered classes name their GType")
    return 1 if missing else 0


if __name__ == "__main__":
    raise SystemExit(main(pathlib.Path(sys.argv[1])))
