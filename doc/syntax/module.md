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
| `resources`  | array      |    no    | Resources owned by this module  |
| `protection` | array      |    no    | Var → lock mapping              |
| `functions`  | array      |    no    | Function definitions            |

`NameSet` (`provides`) is `{ "resources": [...], "functions": [...] }`
(both default `[]`). Short names only.

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
