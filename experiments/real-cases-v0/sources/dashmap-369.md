# Source: xacrimon/dashmap issue #369

- URL: https://github.com/xacrimon/dashmap/issues/369
- Repository: https://github.com/xacrimon/dashmap
- License: MIT
- Retrieved: 2026-09-18
- Reported against: dashmap 6.1.0 (no commit pinned by the reporter)
- State at retrieval: closed (no linked PR/commit shown)

## Mechanism (quoted/condensed from the issue)
"Holding a `Ref` (read guard) from `DashMap::get()` while calling
`DashMap::insert()` with a key that maps to a different shard can cause a
deadlock when another thread performs the reverse operation."

Reproducer (condensed from the issue):
- Thread 0: `let ref_a = m1.get("alpha")` (read shard A) then
  `m1.insert("beta", 10)` (write shard B).
- Thread 1: `let ref_b = m2.get("beta")` (read shard B) then
  `m2.insert("alpha", 20)` (write shard A).

"Thread 0: read(shard_A) held -> write(shard_B) blocked; Thread 1:
read(shard_B) held -> write(shard_A) blocked."

DPOR event trace (quoted from the issue): read(A); write(B); write-release;
read-release; read(B); write(A); write-release; read-release.

## Fix direction (quoted/condensed)
"Drop the `Ref` before mutating other entries" (not a lock reordering).

The full issue text is at the URL above; this excerpt is kept only for provenance.
