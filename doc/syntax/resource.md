# Resource

**Synchronization primitives** (`kind: "sync"`):

```json
{"name": "mtx", "kind": "sync", "type": "Mutex", "mode": "Sync"}
{"name": "sem", "kind": "sync", "type": "Semaphore", "mode": "Async", "count": 3}
{"name": "tx",  "kind": "sync", "type": "Channel", "mode": "Async", "base": "Int", "capacity": 8}
```

| type      |   mode   |  count   |   base   | capacity |
| --------- | :------: | :------: | :------: | :------: |
| Mutex     | required |    —     |    —     |    —     |
| RwLock    | required |    —     |    —     |    —     |
| Condvar   | required |    —     |    —     |    —     |
| Semaphore | required | required |    —     |    —     |
| Channel   | required |    —     | required | required |

**Channel is the message store.** `base` is the payload type of each slot;
`capacity` is the number of in-flight slots (E001 if missing or negative):

- `capacity: 0` — rendezvous (no buffered payload)
- `capacity: n` (`n ≥ 1`) — bounded buffer of `n` messages of type `base`

`channel_send` enqueues into those slots; `channel_recv` ([statement](statement.md)
or [`select` guard](statement.md#select)) dequeues one slot into `dst`. The CVN
currently still treats send/recv as unbuffered tokens; bounded-buffer
semantics from `capacity` are on the roadmap.

**Shared variables** (`kind: "var"`):

```json
{"name": "count", "kind": "var", "type": "Var",    "base": "Int", "init": 0}
{"name": "flag",  "kind": "var", "type": "Atomic", "base": "Bool", "init": false}
```

A string that is not a builtin primitive is a [module type](module.md#named-types)
name (`"Record"` or `"storage::Record"`).

**`base` values**:

| Value                                | Description            | init example |
| ------------------------------------ | ---------------------- | ------------ |
| `"Bool"`                             | Boolean                | `true`       |
| `"Int"`                              | Integer                | `0`          |
| `{"Int": [lo, hi]}`                  | Bounded Int `[lo, hi]` | `3`          |
| `"Float"`                            | Floating-point         | `3.14`       |
| `"String"`                           | String                 | `""`         |
| `{"Enum": ["A","B"]}`                | Enum                   | `"A"`        |
| `{"Struct": {"x":"Int"}}`            | Struct                 | `{"x": 0}`   |
| `{"Array": {"elem":"Int","len":10}}` | Fixed-length array     | `[]`         |

Bounded Int: a CVN update leaving `[lo, hi]` disables the transition, so
counter loops stay finite. Literals outside the domain are E208 / E203.

Var resources that are lock-protected are listed in
[Protection](protection.md).
