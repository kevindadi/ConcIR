# Module

A module is an independently authored fragment: [resources](resource.md),
[protection](protection.md), [functions](function.md), and a name-resolution
contract.

```json
{
  "name": "storage",
  "provides": { "resources": ["log_mtx"], "functions": ["flush"] },
  "requires": { "resources": [], "functions": ["core::log"] },
  "resources": [
    {"name": "log_mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
  ],
  "protection": [],
  "functions": [ ... ]
}
```

| Field        | Type       | Required | Description                     |
| ------------ | ---------- | :------: | ------------------------------- |
| `name`       | ident      |   yes    | Module identity                 |
| `provides`   | NameSet    |    no    | Short names this module exports |
| `requires`   | RequireSet |    no    | FQNs this module imports        |
| `types`      | TypeDef[]  |    no    | Named types owned by this module |
| `resources`  | array      |    no    | Resources owned by this module  |
| `protection` | array      |    no    | Var → lock mapping              |
| `functions`  | array      |    no    | Function definitions            |

`NameSet` (`provides`) is `{ "resources": [...], "functions": [...], "types": [...] }`
(all default `[]`). Short names only.

## Named types

A module may declare types once and reuse them as `base` / param / local
types. A [`BaseType`](resource.md) string that is not `Bool` / `Int` /
`Float` / `String` is a type name: same-module short name, or a FQN
listed in `requires.types`.

```json
{
  "name": "storage",
  "types": [
    { "name": "Record", "type": { "Struct": { "size": "Int", "ready": "Bool" } } }
  ],
  "provides": { "types": ["Record"], "resources": ["shared_map"], "functions": [] },
  "resources": [
    { "name": "shared_map", "kind": "var", "type": "Var", "base": "Record", "init": { "size": 0, "ready": false } }
  ]
}
```

Another module writes `"requires": { "types": ["storage::Record"] }` and
`"base": "storage::Record"`. Builtin names (`Int`, …) cannot be
redeclared (**E112**). Duplicate names are **E110**; an unknown name is
**E111**; a cyclic alias is **E113**.

`RequireSet` (`requires`) is the same shape, but each `functions` entry
is either an FQN string (name-only, backward compatible) or a
[function signature](function.md#concurrency-interface) object:

```json
"requires": {
  "resources": ["storage::log_mtx"],
  "functions": [
    {
      "name": "storage::flush",
      "kind": "normal",
      "may_block": false,
      "locks": { "requires_held": ["log_mtx"] }
    }
  ]
}
```

The signature is what the importing module believes. When the program is
assembled, it must match the defining function (**E804**): `kind`,
`may_block`, `locks`, and any listed `params` / `returns`. A name-only
FQN still satisfies E108 and does not add an interface check.

The validator consumes the assembled [`Program`](program.md). `provides` /
`requires` are enforced (E108): a provided name must be declared here; a
required FQN must exist and be exported by the owning module; a cross-module
call target must appear in `requires.functions`. See [Naming](naming.md).
