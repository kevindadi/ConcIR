# Mapping: xacrimon/dashmap #369 -> CIR

## Source mechanism (from the issue, not from this tool)
Per-shard `RwLock`. Thread 0 holds `read(shard_A)` (a `Ref` from `get("alpha")`)
and calls `insert("beta")`, which takes `write(shard_B)`. Thread 1 holds
`read(shard_B)` and calls `insert("alpha")` -> `write(shard_A)`. Under a schedule
where both reads are taken before both writes, each thread blocks on the other's
write lock: a 2-cycle.

## CIR correspondence
| source | CIR |
| --- | --- |
| reader/inserter thread 0 | function `main::t0` |
| reader/inserter thread 1 | function `main::t1` |
| shard containing `alpha` | resource `main::shard_alpha` (sync **RwLock**) |
| shard containing `beta` | resource `main::shard_beta` (sync RwLock) |
| `get()` guard | `rwlock_read` ... `rwlock_unlock` |
| `insert()` | `rwlock_write` ... `rwlock_unlock` |
| `preserved` | both threads complete |
| safety | `deadlock_free` |

## Omitted / abstracted
- Hashing / shard-selection is abstracted to two named shards; the number of
  shards, table resizing and `Ref`/`RefMut` lifetimes are omitted.
- Only one read guard and one write per thread are kept.
- RwLock read/write preference and reader-writer upgrade are not modelled.

## Ground truth and support boundary
The two-shard opposite-order cycle is taken from the issue's DPOR event trace.
However, **CIR v1 lowerer marks RwLock operations unsupported**
(`src/sem/program.rs`: "RwLock operations are not supported in v1"), so this
faithful model is expected to be `UNSUPPORTED`, not verified. It is included to
document a concrete support boundary rather than being re-wired to `Mutex` (which
would erase the read/write semantics the source depends on).
