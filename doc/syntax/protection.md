# Protection

```json
{ "var": "counter", "lock": "mtx" }
```

Each `Var` appears at most once. `Atomic` resources must not appear here.
See [Resource](resource.md) for `Var` vs `Atomic`.
