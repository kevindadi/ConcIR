# Validation pipeline

Nine passes; each emits diagnostics independently:

```
structure  →  names  →  types  →  compat  →  protection
    E0xx       E1xx      E2xx     E3xx        E7xx

→  concurrency  →  locks  →  control  →  dataflow
       E4xx        E5xx      E6xx         E9xx
```

JSON that does not match this grammar fails at deserialize (E000), including
unknown `kind` tags, a leftover `call` field on a block, or a missing terminator.

See [`error_codes.md`](../error_codes.md) for the full diagnostic catalog.
