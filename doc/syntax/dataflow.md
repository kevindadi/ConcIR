# Data flow (proposal, ConcIR 3.5)

**Status:** in progress. Phases 1–3 are implemented (`src/env.rs`,
`src/expr.rs`, `src/validate/dataflow.rs`, `src/validate/types.rs`,
`src/validate/locks.rs`). Phase 4 (docs, default version 3.5.0) is not
done. Control flow is already a closed CFG ([`statement.md`](statement.md)).
This page closes **names, values, and updates** so every string that
today sits in `expr` / `cond` / `args` / `dst` has a single resolution
and a single type.

## Abstraction (does not change)

These constraints are inputs, not outcomes of this proposal:

- One function body is shared by every activation (no thread-instance
  places). Multiple tokens may sit on the same `sid`.
- `call` of a bodied function expands to the callee skeleton plus a
  wait place. Overlapping calls may hand the return token to the wrong
  site (false positives, not false negatives). That over-approx stays.
- [`Module`](module.md) and FQNs stay. They are how large programs are
  generated and how a name crosses a module boundary.

Under a shared body, **only globally named resources are a sound
concurrent store**. Function locals and parameters are sequential
scratch for one activation. Putting them in the net as a single slot
can *remove* deadlocks (a corrupted local takes the unlock arm). This
proposal therefore splits the two stores instead of pretending a local
is thread-private memory.

## What is not closed today

| Hole | Example | Effect |
| ---- | ------- | ------ |
| Expressions are unparsed strings | `Expr = ? string ?` in [`ebnf.md`](../ebnf.md) | No def-use; types only on literals (E203–E206) |
| Two left-hand sides | `write_shared` uses resource `count`; `channel_recv` writes a local | Same value, two namespaces, no common dst rule |
| `call.dst` ≠ other dsts | E921 requires Var/Atomic; `atomic_cas.dst` may be a local | Returns cannot land in the sequential store |
| Guards bypass protection | E309 watches `read_shared` / `write_shared`, not `branch` on `count` | A guard can read a protected Var without the lock |
| `switch.var` is resource-only | `types.rs` looks up `resource_types` | A local enum cannot be a scrutinee |
| Fork has no data story | `scope` passes `[]`; `spawn.args` are not arity-checked | Modeled params on a spawned function become one shared slot |
| `modeled: false` is a hard error | E912 | Sequential names cannot appear in a guard even as Unknown |
| Struct values are informal | `"expr": "{100, true}"` | Not in the expression grammar the translator actually parses |

## Two stores

```
Concurrent store (always in the CVN)
  Var, Atomic, Channel payload slots
  named by module::entity (short name in-module)
  shared by every token of every function

Activation store (sequential; default not in the CVN)
  Function params, locals, modeled return slot
  named in the function
  one copy per function body — not per token
```

| Store | In the net? | Shared across tokens of the same function? | Typical use |
| ----- | ----------- | ------------------------------------------ | ----------- |
| Resource (Var / Atomic / Channel slot) | yes | yes — this *is* shared memory | Guards and updates that other threads must see |
| Param / local, `modeled: false` (default) | no | n/a | Codegen; sequential arithmetic; dst of recv/cas/call when the value does not need to be in the net |
| Param / local / return, `modeled: true` | yes, **one slot per function** | yes, unsoundly | Last-write-wins. Allowed, but a guard that decides a lock/unlock using this slot is an over-approx that can miss deadlocks. Prefer a Var. |

`modeled` is a **projection** (does this name exist as a CVN variable),
not a third kind of data flow. Defaults:

- `ParamDecl.modeled` / `LocalDecl.modeled` / `returns.modeled` stay
  `false` when omitted (already the serde default).
- Resources have no flag; they are always projected.
- A `modeled: true` activation slot on a **concurrent entry**
  (spawn / `async_call` / `scope` target) is warning **E937**.

Unmodeled names may appear in expressions (**E912 becomes a warning**):
the translator treats that subterm as `Unknown` (both `branch` arms
enabled; the update does not constrain the net). That is false-positive
territory, which matches the call-site over-approx.

## Name environment

Resolved **inside a function** `F` of module `M`, for each name `n`:

1. `"_"` — discard. Legal only as a dst on `channel_recv`,
   `select` `channel_recv`, `atomic_load`, `read_shared`. Not an r-value.
2. A local of `F`.
3. A parameter of `F`.
4. The return slot of `F`, if `returns.name == n` (r-value only after a
   `return` would have written it; as a dst it is illegal — use `return`).
5. If `n` is an FQN listed in `M.requires.resources`, that resource.
6. If `n` is a short name, the resource `M::n`.
7. An enum variant of a `BaseType::Enum` in scope (resource or local
   type, or a variant unique across those types). Used only as a
   literal, never as a dst.

A name must bind to exactly one of 2–6 for a slot, or to 7 for a
literal. Collisions:

- Param/local vs resource: **E910** / **E914** (error), as today for
  params.
- Duplicate locals: **E915**.
- Ambiguous enum variant vs slot: the slot wins; write `"Variant"` as
  a string only if it is a `String` value, otherwise the identifier is
  the variant literal when it does not name a slot.

**R-values that are resources.** Only `Var` and `Atomic` may appear as
r-values. A Mutex, RwLock, Condvar, Semaphore, or Channel name in an
expression is **E934**. Channel *payloads* are not the channel name;
they live in `channel_recv.dst`.

**FQNs in expressions.** Same rule as the rest of ConcIR: in-module
short name; cross-module `module::entity` listed in `requires`.

## Writable slots (unified dst)

Every destination uses the same resolver. A **writable slot** is:

- a local of the enclosing function, or
- a parameter of the enclosing function, or
- a `Var` or `Atomic` resource (short name or FQN), or
- `"_"` on the ops listed above.

| Op | Destination field | Allowed dst |
| -- | ----------------- | ----------- |
| `assign_local` | `target` | local or param (**not** a resource — use `write_shared`) |
| `read_shared` | optional `dst` | writable slot or omitted |
| `atomic_load` | `dst` | writable slot |
| `atomic_cas` | `dst` | writable slot (old value, same type as Atomic `base`) |
| `channel_recv` / select guard | `dst` | writable slot |
| `call` | optional `dst` | writable slot (**E921 changes**: local is legal) |
| `write_shared` | `resource` | Var only (already E308) |
| `atomic_store` | `resource` | Atomic only |

`assign_local` stays the sequential store. Writing a Var through
`assign_local` is **E936**; writing a local through `write_shared` stays
E308.

Omitted `read_shared.dst` means "this statement is an instantaneous
read of the concurrent store for ordering / E309; subsequent
expressions name the resource directly". Both of these are legal and
mean different stores:

```json
{ "sid": "s2", "kind": "read_shared", "resource": "count" },
{ "sid": "s3", "kind": "branch", "cond": "count > 0", "then": "s7", "else": "s4" }
```

```json
{ "sid": "s2", "kind": "read_shared", "resource": "count", "dst": "tmp" },
{ "sid": "s3", "kind": "branch", "cond": "tmp > 0", "then": "s7", "else": "s4" }
```

The first form is the one the net can decide (resource `count`).
The second form is sequential: if `tmp` is unmodeled, the branch is
`Unknown` in the CVN. **For data-dependent concurrent control, name
the resource in the guard.**

## Expression language

JSON keeps strings (LLM- and file-friendly). The validator **parses**
them. Concrete syntax is a subset of C-like expressions; it is the
same subset `cir2cvn` already lowers (`+ - * / %`, comparisons), plus
field projection so `Struct` types are not a dead end.

```ebnf
Expr       = CmpExpr ;

CmpExpr    = AddExpr
           | AddExpr, CmpOp, AddExpr ;

CmpOp      = "==" | "!=" | "<" | "<=" | ">" | ">=" ;

AddExpr    = MulExpr, { AddOp, MulExpr } ;
AddOp      = "+" | "-" ;

MulExpr    = Unary, { MulOp, Unary } ;
MulOp      = "*" | "/" | "%" ;

Unary      = [ "-" ], Atom ;

Atom       = Literal
           | Slot
           | Field
           | "(", Expr, ")" ;

Slot       = Name ;                 (* resolved by the name environment *)

Field      = Atom, ".", Ident ;     (* Struct field; Atom must have Struct type *)

Literal    = "true" | "false"
           | Integer
           | Float
           | StringLiteral
           | Ident ;                (* enum variant, if not a slot *)

Name       = Ident | Fqn ;
```

Out of v1 (keep as later extensions, not holes in the concurrent
store): `&&` / `||`, indexing into `Array`, function calls inside
expr, struct *positional* literals.

**Struct construction** (needed because examples already write
aggregate Vars):

```ebnf
StructLit  = "{", [ Ident, ":", Expr, { ",", Ident, ":", Expr } ], "}" ;
```

`StructLit` is an additional `Atom`. Every field of the target Struct
type must be present (**E933** if a field is missing or unknown).
Positional forms such as `"{100, true}"` become **E931**.

**Conditions.** `branch.cond` must parse as a `CmpExpr` whose result
type is Bool (**E201** stays, but is now parser-backed rather than
`contains("==")`). No bare identifier as a condition (`ready` is
illegal; write `ready == true`).

**Switch scrutinee.** `switch.var` is a `Name`, not a general expr. It
must resolve to a slot whose type is Enum, `Int`, or bounded `Int`
(**E935**; replaces the resource-only lookup behind E202).

### Type rules (sketch)

After parse + name resolution, each expr has a `BaseType` or is
`Unknown` (unmodeled slot).

- Literals: Bool / Int / Float / String / Enum as today.
- Slot: declared type of the local, param, Var, or Atomic.
- Field: field type of a Struct; **E933** otherwise.
- `+ - * / %`: both sides Int or both bounded Int (result Int / the
  join of the two bounds); mixing with Float is **E932**.
- Comparisons: operands of the same type; result Bool.
- Bounded Int: a constant outside `[lo, hi]` is still E203/E204/E205;
  a non-constant update that may leave the domain is a translator
  concern (transition disabled), not a validator error.
- `write_shared` / `atomic_store` / `channel_send` / `atomic_cas`
  expected/desired / `return.value` / `call` args: expr type must
  match the destination type (**E203–E206**, **E932** for args).

Unmodeled slots contribute `Unknown`. `Unknown` is accepted in any
position and does not satisfy exhaustiveness or bounded-Int proofs; it
only disables precision.

## Sequential call vs concurrent entry

### `call`

Sequential. Args and dst are real data flow in the caller.

- `args` : one `Expr` per **modeled** parameter of the callee, in
  declaration order (**E920**). Evaluated in the **caller** environment.
  Unmodeled callee parameters are codegen-only and do not appear in
  `args` (unchanged).
- `dst` : writable slot in the caller (**E921**, unified). If the
  callee has a modeled return, omitting `dst` drops the value. If the
  callee has no modeled return, `dst` is **E923**.

Binding modeled params still uses one CVN slot per callee function
(`p_{fn}_{param}`). Overlapping calls remain the accepted
over-approx.

### `spawn` / `async_call` / `scope`

These put a **second token** on a shared body. Modeled parameters are
not a sound mailbox.

- A function named by `spawn`, `async_call`, or `scope.funcs` must have
  **zero modeled parameters** (**E922**). Pass concurrent inputs
  through resources (a Var, an Atomic, a Channel).
- `scope` has no `args` field (unchanged).
- `spawn` / `async_call` `args` may still list strings for unmodeled
  parameters (codegen). They are not projected. Arity, if present,
  matches the unmodeled parameter list (**E924**); empty `args` is
  allowed when every parameter is unmodeled or there are no
  parameters.

Repeating the same function N times remains a `branch` loop of `spawn`,
not a count on `scope`.

## Protection × expressions

E309 today: `read_shared` / `write_shared` of a protected Var without
holding the lock.

**Extend E309 to every resolved r-value and write of that Var in the
statement**, including:

- `branch.cond`, `write_shared.expr`, `atomic_cas` expected/desired,
  `channel_send.value`, `assign_local.expr`, `return.value`, `call` args
  that name the Var;
- `switch.var` when it is that Var.

`read_shared` with the lock held still covers the "I intend to read
this now" form. A `branch` on `count` *is* a read of `count`.

Atomic resources stay out of `protection` (E703). Channel payloads
are protected by the channel resource itself, not by a mutex, unless
the dst is a protected Var (then the write to the Var needs the lock).

## Error catalog (E9xx target)

| Code | Name | Severity | Change |
| ---- | ---- | :------: | ------ |
| E910 | ParamNameCollides | error | unchanged |
| E911 | DuplicateParam | error | unchanged |
| E912 | UnmodeledNameInNetExpr | **warning** | no longer an error; name is `Unknown` in the CVN |
| E913 | BareReturnWithModeledReturn | warning | unchanged |
| E914 | LocalNameCollides | error | **new** — local vs resource or vs param |
| E915 | DuplicateLocal | error | **new** |
| E920 | CallArityMismatch | error | still `call` × modeled params |
| E921 | DstNotWritableSlot | error | **broadened** — local/param/Var/Atomic/`_` as specified |
| E922 | ModeledParamOnConcurrentEntry | error | **new** — spawn / async_call / scope target |
| E923 | DstWithoutModeledReturn | error | **new** |
| E924 | SpawnArityMismatch | error | **new** — optional; unmodeled params only |
| E931 | ExprParseError | error | **new** |
| E932 | ExprTypeMismatch | error | **new** — non-literal cases E2xx do not cover |
| E933 | BadProjection | error | **new** — missing/unknown struct field |
| E934 | NonValueResourceInExpr | error | **new** |
| E935 | SwitchScrutineeNotSlot | error | **new** — also covers locals; E202 remains for bad types |
| E936 | AssignLocalToResource | error | **new** |
| E937 | ModeledActivationOnConcurrentEntry | warning | **new** — modeled local/return on a spawn target |

E2xx keep their codes for resource-typed writes (E203–E206, E201).
Once the parser exists, E201 is "condition is not a Bool `CmpExpr`"
rather than "string lacks a comparison operator".

## JSON compatibility

No new top-level fields. No change to statement `kind` tags.
Straight-line programs that already write `count + 1` and
`count > 0` parse unchanged.

Required edits when this lands:

- `"expr": "{100, true}"` → `"{size: 100, ready: true}"` (field names
  from the Struct type). Touches `examples/complex_rwlock.json`.
- `call` into a function with a modeled return may capture into a
  local; existing captures into a Var remain valid.
- A `scope`/`spawn` of a function that currently has modeled params
  must move those inputs to resources or mark the params unmodeled.
- Programs that referenced unmodeled params to *force* E912 will
  become warnings; tests in `tests/validate_dataflow.rs` must follow
  the new severity.

Default `Program.version` becomes `"3.5.0"` in the same change set
that turns these rules on. Older files without `version` keep
deserializing; the new checks apply to every program (no
mode-by-version fork).

## Worked examples

### Resource guard (preferred concurrent form)

From `examples/producer_consumer.json` — already in the closed shape:

```json
{ "sid": "s2", "kind": "read_shared", "resource": "count" },
{ "sid": "s3", "kind": "branch", "cond": "count > 0", "then": "s7", "else": "s4" }
```

`count` is a Var in the concurrent store. E309 applies to both
statements once protection×expr lands. `read_shared` without `dst` is
not redundant: it is the modeled read event; the branch names the
same slot.

### Recv payload then sequential branch

```json
"locals": [{ "name": "msg", "type": "Int", "modeled": false }],
"body": [
  { "sid": "s1", "kind": "channel_recv", "channel": "tx", "dst": "msg" },
  { "sid": "s2", "kind": "branch", "cond": "msg > 0", "then": "s3", "else": "s4" }
]
```

Legal. `msg` is sequential. The net sees recv (channel tokens) and a
nondeterministic branch (E912 warning). To have the payload decide a
*concurrent* path, recv into a Var instead:

```json
{ "sid": "s1", "kind": "channel_recv", "channel": "tx", "dst": "last_msg" }
```

where `last_msg` is a Var (protected if other threads read it).

### Unified call dst

```json
{ "sid": "s1", "kind": "call", "func": "process", "args": ["budget", "10"], "dst": "tmp" }
```

`tmp` may be a local. Today this is E921; after closure it is legal.
If `process` must publish the result to another thread, `dst` is a Var.

### Concurrent entry: no modeled params

Illegal after E922:

```json
{ "sid": "s1", "kind": "scope", "funcs": ["worker"] }
```

with `worker.params = [{ "name": "n", "type": "Int", "modeled": true }]`.

Legal: `worker` reads a Var `budget` that `main` wrote before the
`scope`, or `worker` has only unmodeled params.

## Implementation phases

Do not land a half-parser behind the old E912 error. Each phase should
compile and keep `cargo test` green.

### Phase 1 — Name environment and unified dst

- Add `src/env.rs` (or `validate/env.rs`): per-function slot table
  (locals, params, return name, in-scope resources).
- Resolve every `dst` / `assign_local.target` / `switch.var` through
  that table. Implement E914, E915, E921 (new meaning), E936, E935.
- Extend E920-style arity to `spawn` / `async_call` as E924; add E922.
- Rewrite `tests/validate_dataflow.rs` for the new E921/E912.
- Files: `src/validate/dataflow.rs`, `src/validate/types.rs` (switch),
  `doc/error_codes.md`.

### Phase 2 — Expression parser in this crate

- Add `src/expr.rs`: `Expr` AST, parser, `type_of(expr, env)`.
- Keep the string in JSON; parse at validation time. Do not change
  serde shapes.
- Wire E201, E931, E932, E933, E934 onto parsed trees. Literal
  bounded-Int checks stay in E2xx.
- Share the grammar with `cir2cvn` later by making this module the
  source of truth (translator currently owns a copy in
  `src/translator/expr_parser.rs`).
- Unit tests for parse/type; re-parse every `examples/*.json` expr.

### Phase 3 — Protection on expr reads

- In the E309 worklist, collect resource r-values from the parsed expr
  of the current statement (plus existing `read_shared`/`write_shared`).
- Same held-set algorithm as `validate/locks.rs`.
- Test: `branch` on a protected Var without lock → E309; producer-
  consumer remains valid.

### Phase 4 — Docs and examples

- Fold this page into the normative grammar: replace the `Expr = ?
  string ?` comment in [`ebnf.md`](../ebnf.md); point
  [`function.md`](function.md) at this page for `modeled` / dst / args.
- Fix `examples/complex_rwlock.json` struct literals.
- Default version `3.5.0`.
- Downstream `cir2cvn`: drop the duplicate parser; honour E922 (do not
  bind modeled params at spawn); project unmodeled names as `Unknown`.

Phase 1 is enough for dst/call/spawn to stop lying. Phase 2 is the
actual closure. Phase 3 is required for E309 to match the name
environment. Phase 4 is the public contract bump.

## Out of scope

- Thread-instance places / per-token locals. Explicitly rejected.
- Channel `capacity` in the CVN (roadmap item; the IR field is already
  the representation).
- Replacing fallthrough CFG with old `op` + `transfer`.
- `&&` / `||`, array index, pointer/aliasing.
- Diagnostic `location: module::function.sid` (orthogonal; should
  still happen, not in this change set).

## Downstream note

`cir2cvn` already parses the arithmetic/comparison subset and aliases
modeled params to CVN variables. After Phase 2 it should call ConcIR's
parser so a program that validates cannot fail translation on expr
syntax. Field projection and struct literals are new work on both
sides; until Phase 2 lands, the translator may keep rejecting
`shared_map.ready`.
