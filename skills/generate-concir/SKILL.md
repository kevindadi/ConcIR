---
name: generate-concir
description: Generate or repair ConcIR JSON concurrency skeletons from prose or code. Use when the user wants ConcIR, CIR, a seq_hole fill-in, cir validation fixes, or a concurrent IR program.
metadata:
  concir: "3.5.0"
---

# Generate ConcIR

ConcIR is a statement-level concurrency IR. Produce a **concurrency skeleton**
that `cir` accepts. Sequential interiors go in `seq_hole` (or a nobody
function). Do not invent thread instance ids.

## Locate ConcIR

Need the grammar and the validator.

1. If `CONCIR_ROOT` is set, use it.
2. Walk parents of the current workspace for `doc/syntax/README.md` and
   `src/ast.rs`.
3. If this skill is still inside a ConcIR checkout (including a symlink),
   the repo root is two directories above this `SKILL.md`.

Docs (from that root):

| Topic | Page |
| ----- | ---- |
| Naming / FQN / `location` | `doc/syntax/naming.md` |
| Program | `doc/syntax/program.md` |
| Module, `types`, signed `requires` | `doc/syntax/module.md` |
| Resources / Channel | `doc/syntax/resource.md` |
| Protection | `doc/syntax/protection.md` |
| `may_block` / `locks` / `bound` | `doc/syntax/function.md` |
| Statements, `seq_hole` | `doc/syntax/statement.md` |
| dst / expr / call vs spawn | `doc/syntax/dataflow.md` |
| Error codes | `doc/error_codes.md` |

Few-shot: `examples/producer_consumer.json`, `examples/async_workers.json`.
Do not paste the EBNF into the answer.

## Workflow

1. Name **roles** (functions), not thread instances.
2. Emit modules + `provides` / `requires`.
3. Declare `types`, `resources`, `protection`.
4. Write function interfaces (`kind`, `may_block`, `locks`, optional `bound`).
5. Write the skeleton: locks, wait, send/recv, `scope`/`spawn`, `call`, CFG.
6. Leave sequential work as `seq_hole` (or empty `body` + interface).
7. Validate. Fix by `diagnostics[].location` (`module::function.sid`).
8. Fill holes only after the skeleton is `valid: true`.

```text
roles → modules → types/resources/protection
     → interfaces → skeleton + seq_hole
     → cir → repair by location → fill holes
```

Deliver a `.json` file, not a Markdown fence as the only artifact.

## Hard constraints

- **No thread instance ids.** Every activation shares one function body.
  `bound` is a role multiplicity (token cap), not an instance name.
  Repeat a role N times with a `branch` loop of `spawn`, never a count on
  `scope`. `bound < 1` is E960; `bound` on a never-spawned function is E961.
- **Modules are required.** Same-module short names. Cross-module names are
  FQNs (`module::entity`) and must appear in `requires`. `provides` is short
  names. `entry` is an FQN.
- **`seq_hole` is sequential only.** Footprint is Var/Atomic (E310). Protected
  Vars still need the lock (E309). Do not put lock / wait / send / spawn
  inside a hole. `id` is an ident, unique per function (E006, E109).
- **`abstract_step` enters the net.** Use it for an opaque *modeled* step,
  not as an LLM fill site. Nobody (`body: []`) is a whole-function
  placeholder; its spec is `may_block` / `locks` / `effects`.
- **Concurrent entries take no modeled params (E922).** Shared state is a
  resource. Modeled locals on a spawn target are warning E937.
- **Interfaces must match the body.** `may_block: false` on a blocking body
  is E802. A `call` must already hold the callee's `requires_held` (E803).
  An imported signature must match the definition (E804). Lock-effect names
  are Mutex/RwLock (E801).
- **CFG.** `sid` is `"s"` plus digits. Non-control ops fall through — do not
  `goto` the next statement. Every path ends in `return` (E602).
- **Version** is `"3.5.0"`.

## Minimal shape

```json
{
  "program": "app",
  "version": "3.5.0",
  "modules": [
    {
      "name": "core",
      "provides": { "resources": ["mtx", "flag"], "functions": ["main", "worker"] },
      "requires": { "resources": [], "functions": [] },
      "resources": [
        { "name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync" },
        { "name": "flag", "kind": "var", "type": "Var", "base": "Bool", "init": false }
      ],
      "protection": [{ "var": "flag", "lock": "mtx" }],
      "functions": [
        {
          "name": "main",
          "kind": "normal",
          "body": [
            { "sid": "s1", "kind": "scope", "funcs": ["worker"] },
            { "sid": "s2", "kind": "return" }
          ]
        },
        {
          "name": "worker",
          "kind": "normal",
          "may_block": false,
          "locks": { "acquires": ["mtx"], "releases": ["mtx"] },
          "body": [
            { "sid": "s1", "kind": "mutex_lock", "resource": "mtx" },
            { "sid": "s2", "kind": "seq_hole", "id": "update_flag", "desc": "sequential update", "reads": ["flag"], "writes": ["flag"] },
            { "sid": "s3", "kind": "mutex_unlock", "resource": "mtx" },
            { "sid": "s4", "kind": "return" }
          ]
        }
      ]
    }
  ],
  "entry": "core::main"
}
```

Cross-module `requires.functions` may be an FQN string or a signature object
(`name`, `kind`, `may_block`, `locks`, optional `params` / `returns`).
Named types live in `module.types` and are exported/imported like resources.

## Validate and repair

From `CONCIR_ROOT`:

```bash
cargo run --quiet -- path/to/program.json
```

or `cir path/to/program.json` if that binary is on `PATH`.

Success is `{ "valid": true, "diagnostics": [] }` (warnings may still appear).
On failure, edit the statement named by `location`. `path` is secondary.
Do not guess a fix for an error code you have not looked up in
`doc/error_codes.md`.

Common first-pass mistakes: missing `return`; `goto` to the next sid;
FQN used inside its own module; lock/wait inside `seq_hole`; modeled
params on a `scope` target; `condvar_wait` as a `select` guard on a
sync function (E409).
