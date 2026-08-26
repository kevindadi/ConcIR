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

| Field        | Type    | Required | Description                     |
| ------------ | ------- | :------: | ------------------------------- |
| `name`       | ident   |   yes    | Module identity                 |
| `provides`   | NameSet |    no    | Short names this module exports |
| `requires`   | NameSet |    no    | FQNs this module imports        |
| `resources`  | array   |    no    | Resources owned by this module  |
| `protection` | array   |    no    | Var → lock mapping              |
| `functions`  | array   |    no    | Function definitions            |

`NameSet` is `{ "resources": [...], "functions": [...] }` (both default `[]`).

The validator consumes the assembled [`Program`](program.md). `provides` /
`requires` are enforced (E108): a provided name must be declared here; a
required FQN must exist and be exported by the owning module; a cross-module
call target must appear in `requires.functions`. See [Naming](naming.md).
