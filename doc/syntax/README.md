# ConcIR Syntax

ConcIR (Concurrency Intermediate Representation) is a statement-level,
verification-oriented IR. A function body is a flat list of statements
(CFG nodes); control transfer is itself a statement kind (`goto` /
`branch` / `switch` / `return` / `select`). ConcIR is language-neutral:
names are ConcIR identifiers and FQNs, never backend crate paths or
source-language keywords.

The executable definition is `src/ast.rs`, `src/fqn.rs`, `src/env.rs`,
`src/expr.rs`, and `src/validate/` (including `interface.rs` for E8xx).
The formal grammar is [`ebnf.md`](../ebnf.md). See
[`error_codes.md`](../error_codes.md) for diagnostics and
[`todo.md`](../todo.md) for the roadmap.

[Data flow](dataflow.md) is the ConcIR 3.5 account of names,
destinations, expressions, and call vs spawn. [`function.md`](function.md)
and E9xx match that page.

| Page | Topic |
| ---- | ----- |
| [Naming](naming.md) | Identifiers, entity FQNs, control locations |
| [Program](program.md) | Top-level `{ program, version, modules, entry }` |
| [Module](module.md) | `provides` / `requires`, types, resources, functions |
| [Resource](resource.md) | Sync primitives, shared vars, `base` types, Channel |
| [Protection](protection.md) | Var → lock mapping |
| [Function](function.md) | `kind` / `form`, params / returns / locals |
| [Statement](statement.md) | CFG node: data, sync, threads, calls, control ops |
| [Data flow](dataflow.md) | Two stores, unified dst, expression language, call vs spawn |
| [Validation](validation.md) | Nine-pass pipeline |
