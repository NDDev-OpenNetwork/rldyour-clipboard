# Clipboard 0.2 quality and architecture

The Rust daemon owns policy, transport/session admission, native capture,
maintenance and persistence in separate modules. Clients never open the index.
SQLite operations are serialized; content I/O and expensive image derivation
run outside that lock. A blob lease covers publication through index commit,
and garbage collection rechecks references under the index lock. Failed and
abandoned drafts release leases and remove unreferenced bytes. A process-wide
archive file lock also prevents a second daemon from sweeping incoming files.

Pins use a FULL-synchronous commit so the explicit keep action synchronizes WAL.
Ordinary capture uses NORMAL with SQLite's default checkpoints for lower write
cost; recent unpinned captures may roll back after power loss. The archive must
stay on a local filesystem. Bundled SQLite 3.53.2 includes the WAL-reset fix
that SQLite documents for older releases.

Image decode is serialized, with a 16-megapixel / 64 MiB decoder allocation
ceiling and 64 MiB encoded-input ceiling. Oversized originals are kept but may
have no thumbnail/transcode. Thumbnail decoding is independently bounded to
256-pixel edges. This is a policy bound, not a guarantee about total process
memory: decoder implementations, capture APIs and simultaneous I/O add cost.
Linux's service also has a 256 MiB memory limit. Native capture polls the macOS
change count every 500 ms; Windows/X11 are event-driven. Maintenance wakes once
a minute. The Mac UI queries on demand and has no continuous polling timer.

Tests use fresh temporary archives with native capture disabled. They exercise
strict age boundaries, pin/unpin, restart, shared-content GC, draft leases,
second-daemon refusal, byte-exact restore, multipart serialization, bounded
headers and slow subscribers. Native CI compiles/tests macOS ARM/Intel,
Windows and Linux; GJS integration uses the real socket without a running
Shell. That validates transport and static GNOME compatibility, not a live
visual test of every GNOME version. The macOS QA build uses synthetic entries
and disables writes to the actual system pasteboard.

## Primary references checked 2026-10-07

- [SQLite WAL, durability and WAL-reset bug](https://www.sqlite.org/wal.html).
- [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
- [Apple NSPasteboard](https://developer.apple.com/documentation/appkit/nspasteboard).
- [GNOME extension best practices and teardown](https://gjs.guide/extensions/review-guidelines/best-practices.html).
- [GNOME review guidelines](https://gjs.guide/extensions/review-guidelines/review-guidelines.html).

Release builds use Cargo.lock (`--locked`) and SHA-pinned Actions. A release
version gate checks daemon/Python/changelog agreement before building, and
checksums accompany all GitHub release assets. Python is dependency-free at
runtime; image decoding remains an optional Rust feature.
