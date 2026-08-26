# ConcIR Syntax

ConcIR (Concurrency Intermediate Representation) is a statement-level,
verification-oriented IR. The shape follows compiler intermediate code
(basic blocks, statements, calls, terminators) rather than a source language.
ConcIR is language-neutral: names are ConcIR identifiers and FQNs, never
backend crate paths or source-language keywords.

The executable definition is `src/ast.rs`, `src/fqn.rs`, and `src/validate/`.
The formal grammar is [`ebnf.md`](../ebnf.md). See
[`error_codes.md`](../error_codes.md) for diagnostics and
[`todo.md`](../todo.md) for the roadmap.

| Page | Topic |
| ---- | ----- |
| [Naming](naming.md) | Identifiers, entity FQNs, control locations |
| [Program](program.md) | Top-level `{ program, version, modules, entry }` |
| [Module](module.md) | `provides` / `requires`, resources, functions |
| [Resource](resource.md) | Sync primitives, shared vars, `base` types, Channel |
| [Protection](protection.md) | Var → lock mapping |
| [Function](function.md) | `kind` / `form`, scope, params / returns / locals |
| [Basic block](block.md) | Flattened CFG: statements + terminator |
| [Statement](statement.md) | Data, sync, threads, calls |
| [Terminator](terminator.md) | `goto` / `branch` / `switch` / `return` / `select` |
| [Validation](validation.md) | Nine-pass pipeline |
| [Wait semantics](wait.md) | `condvar_wait` lock release / re-acquire |
