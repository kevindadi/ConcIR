# Top-level program

A program is a set of [modules](module.md) plus one entry FQN. There is no
`goals` field; reachability queries belong to the verifier / CVN layer, not
the IR.

```json
{
  "program": "app",
  "version": "3.2.0",
  "modules": [ ... ],
  "entry": "core::main"
}
```

| Field     | Type   | Required | Description                              |
| --------- | ------ | :------: | ---------------------------------------- |
| `program` | string |   yes    | Program name                             |
| `version` | string |    no    | Defaults to `"3.2.0"`                    |
| `modules` | array  |   yes    | One or more [`Module`](module.md)s       |
| `entry`   | FQN    |   yes    | Entry function, e.g. `core::main`        |

`entry` follows the [FQN rules](naming.md).
