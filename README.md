# ConcIR

Static validator for **ConcIR** (Concurrency Intermediate Representation) — a
statement-level, verification-oriented concurrency IR. Reads a ConcIR JSON
file, runs nine validation passes, and emits a structured diagnostic report.
The downstream `cir2cvn` translator builds a CVN (Concurrency Verification Net)
from a validated program for state-space exploration.

ConcIR is language-neutral. A program is a set of modules; names are ConcIR
FQNs (`module::entity`), not backend crate paths. Function bodies are a
flattened CFG: a list of statements. Non-control ops fall through to the
next statement; `goto` / `branch` / `switch` / `return` / `select` are
statement kinds that transfer control.

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
| [`doc/syntax/`](doc/syntax/README.md) | Prose grammar, split by top-level construct |
| [`doc/ebnf.md`](doc/ebnf.md)     | ISO EBNF abstract syntax                                             |
| [`doc/error_codes.md`](doc/error_codes.md) | Validation error reference (E0xx–E9xx) and diagnostic output format |
| [`doc/todo.md`](doc/todo.md)     | Roadmap: modeling scope, call semantics, modular generation     |

## Project structure

```
src/
  main.rs              Entry: read JSON → deserialize → validate → emit report
  lib.rs               Module declarations
  ast.rs               IR types (Program, Module, Stmt, Op)
  fqn.rs               Identifier and FQN rules
  diagnostic.rs        Diagnostic types (Diagnostic, ValidationReport)
  validate/
    mod.rs             Validation entry: chains the 9 passes
    structure.rs       E0xx  Structural validity
    names.rs           E1xx  Name resolution and module contracts
    types.rs           E2xx  Type checking
    compat.rs          E3xx  Resource–operation compatibility
    protection.rs      E7xx  Protection mapping
    concurrency.rs     E4xx  Handle pairing, scope statement
    locks.rs           E5xx  Lock safety (includes E309)
    control.rs         E6xx  Control flow
    dataflow.rs        E9xx  Typed params / returns / call arity
  export/
    dot.rs             Graphviz DOT from statements / calls / control edges
doc/
  syntax/              ConcIR grammar (one page per top-level construct)
  ebnf.md              ISO EBNF abstract syntax
  error_codes.md       Error code reference and diagnostic format
  todo.md              Roadmap
examples/
  producer_consumer.json    Producer–consumer
  async_workers.json        Async tasks + semaphore + Channel
  with_summary.json         Body-less function call chain
  state_machine.json        State machine + Switch
  complex_rwlock.json       RwLock + Condvar combined example
```
