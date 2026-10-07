#!/usr/bin/env python3
"""Hash public release artifacts; never include the output manifest itself."""
import hashlib
from pathlib import Path
import sys
root = Path(sys.argv[1])
lines = []
for path in sorted(root.iterdir()):
    if not path.is_file() or path.name == "SHA256SUMS":
        continue
    with path.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    lines.append(f"{digest}  {path.name}\n")
(root / "SHA256SUMS").write_text("".join(lines))
