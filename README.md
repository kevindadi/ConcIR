# ConcIR

Static validator for **ConcIR** (Concurrency Intermediate Representation) — a
statement-level, verification-oriented concurrency model. Reads a ConcIR JSON
file, runs 8 validation passes, and emits a structured diagnostic report. The
downstream `cir2cvn` translator builds a CVN (Concurrency Verification Net) from
a validated program for state-space exploration.

## Quick Start

```bash
cargo build --release
./target/release/cir examples/producer_consumer.json
```

Output is a JSON `ValidationReport`:

```json
{
  "valid": true,
  "diagnostics": []
}
```

If there are errors, `valid` is `false`, `diagnostics` contains all diagnostic
items, and the process exits with exit code 1.

## Documentation

| Document                         | Contents                                                        |
| -------------------------------- | --------------------------------------------------------------- |
| [`doc/syntax.md`](doc/syntax.md) | ConcIR grammar: top-level shape, resources, functions, ops, transfers, goals, validation pipeline, `wait` semantics |
| [`doc/error_codes.md`](doc/error_codes.md) | Validation error reference (E0xx–E7xx) and diagnostic output format |
| [`doc/todo.md`](doc/todo.md)     | Roadmap: modeling scope, call semantics, modular generation     |

## Project structure

```
src/
  main.rs              Entry: read JSON → deserialize → validate → emit report
  lib.rs               Module declarations
  ast.rs               IR type definitions (Program, Resource, Op, Transfer, etc.)
  diagnostic.rs        Diagnostic types (Diagnostic, ValidationReport)
  validate/
    mod.rs             Validation entry: chains the 8 passes
    structure.rs       E0xx  Structural validity
    names.rs           E1xx  Name resolution
    types.rs           E2xx  Type checking
    compat.rs          E3xx  Resource–operation compatibility
    protection.rs      E7xx  Protection mapping
    concurrency.rs     E4xx  Concurrency pairing
    locks.rs           E5xx  Lock safety (includes E309)
    control.rs         E6xx  Control flow
doc/
  syntax.md            ConcIR grammar reference
  error_codes.md       Error code reference and diagnostic format
  todo.md              Roadmap
examples/
  producer_consumer.json    Producer–consumer
  async_workers.json        Async tasks + semaphore + Channel
  with_summary.json         Body-less function call chain
  state_machine.json        State machine + Switch
  complex_rwlock.json       RwLock + Condvar combined example
```
