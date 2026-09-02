# Naming: identifiers and FQNs

| Form             | Pattern                             | Example                          |
| ---------------- | ----------------------------------- | -------------------------------- |
| Identifier       | `[A-Za-z_][A-Za-z0-9_]*`            | `storage`, `main`, `log_mtx`     |
| Entity FQN       | `module::entity` (exactly one `::`) | `storage::log_mtx`, `core::main` |
| Control location | `module::function.sid`              | `core::main.s3`                  |
| Statement id     | `"s"` + digits                      | `s1`, `s10`                      |

Rules:

1. A **module name** is an identifier. It is ConcIR's own namespace, not a
   Rust crate or Java package.
2. An **entity FQN** names a resource or function as `module::entity`. Extra
   `::` segments are illegal (`crate::foo::bar` is not a ConcIR FQN).
3. A **control location** is `module::function.sid`. Use this when referring
   to a statement from outside the function.
4. **Same-module references use the short name.** Inside module `core`, write
   `main` and `log_mtx`, not `core::main`.
5. **Cross-module references must be FQNs** and must appear in the importing
   module's `requires`.
6. **`provides` always uses short names** declared in this module.
7. **`requires.resources` always uses FQNs.** `requires.functions` is either
   an FQN string or a [signature object](function.md#concurrency-interface)
   whose `name` is that FQN.
8. **`entry` is always an FQN.**
