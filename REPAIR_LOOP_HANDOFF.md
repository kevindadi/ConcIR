# REPAIR_LOOP_HANDOFF

Phase: diagnostic-driven composite patch search, exportable artifacts, and a
development benchmark. This revision addresses the independent review of the
round-3 working tree (E1–E8). Baseline before this revision: `29ee9ed`,
208 passed / 4 failed (the 4 DOT snapshots). After: **222 passed / 0 failed**,
zero warnings.

This is a development regression set and a reviewable implementation; it is
**not** an independent evaluation corpus and makes no formal-proof or
global-optimality claim.

## 1. Baseline convergence (historical DOT failures)

The four `tests/dot_export.rs` snapshot failures came from `.gitignore`'s
`**.snap` excluding the committed goldens while the pending `.snap.new` files
were tracked. The goldens were generated, inspected against the intended graph
structure, and committed under `tests/snapshots/*.snap`; `.gitignore` now
ignores only `*.snap.new` / `*.pending-snap`. `cargo test --test dot_export`
passes 19/19.

Toolchain: `rustc 1.100.0-nightly`, macOS, `cargo 1.100.0-nightly`.

## 2. Fixes E1–E8

### E1 — complex-type serialization (P1)
`ComplexBaseType::Serialize` emitted tuple arrays (`["Int",[0,1]]`) while the
deserializer requires a single-key object. It now serializes a one-entry map
(`src/ast.rs`), covering bounded Int, Enum, Struct, Array, and nesting in
resources, params, locals, and returns. Tests:
`tests/ast_roundtrip.rs` (value-preserving Program round-trip, direct
`ComplexBaseType` round-trip with nesting) and
`tests/repair_search.rs::e1_complex_types_survive_search_export` (a repair over
a program carrying all four complex types exports, reloads, and re-verifies the
frozen contract to a complete PASS).

### E2 — single source of verification bounds (P1)
`SearchConfig.bounds` was silently ignored. It is removed; the frozen
`ContractSpec.bounds` is authoritative for every root and child verification,
and the effective bounds are recorded in the artifact
(`effective_config.bounds`). `e2_bounds_come_from_the_frozen_contract` shows a
tiny contract bound yields `AnalysisUnknown` with a bounded state count.

### E3 — dedup before verification (P2)
Candidate programs are fingerprinted and checked against the cache **before**
`verify_program` and before the verification budget is consumed. Reused
attempts record `reused_node` and are not re-enqueued. Counts distinguish
proposals, unique candidate programs, verification calls, and cache hits.
`e3_dedup_avoids_repeated_verification` asserts `verifications ==
unique_programs` and `cache_hits >= 1` on `two_cycles` (B).

### E4 — node/attempt identity (P2)
Reports now use stable node ids (indices into `nodes`) with `parent` as a node
id; every candidate proposal is a separate `AttemptReport` referencing the node
it came from. Rejected attempts produce no node and are not fake roots.
`e4_attempts_and_nodes_have_consistent_identity` and
`e4_modules_denied_attempts_reference_the_root` cover the two review
counterexamples.

### E5 — budget boundaries and stop reasons (P2)
The root verification counts toward `verification_budget`; a zero budget is
rejected before any verification (`InvalidConfig`, no state built). Depth/edit
truncation, candidate-budget exhaustion, and verification-budget exhaustion
have distinct stop reasons. `e5_verification_budget_zero_is_rejected_before_work`
and `e5_depth_and_edits_truncation_are_reported` cover the boundaries.

### E6 — legacy CLI budget index (P2)
`repair model contract patches.json [budget]` now reads the budget at the
correct position; omitted/0/1/invalid/extra arguments are handled.
`tests/cli.rs::e6_legacy_budget_is_read_and_validated`.

### E7 — repair exit codes (P2)
`repair` maps `RepairOutcome` to the documented codes (UNKNOWN→3, INVALID and
`InvalidConfig`→4, UNSUPPORTED→5), matching `explore`, for all strategies.
`tests/cli.rs::e7_strategy_exit_codes_match_explore`.

### E8 — complete, replayable artifacts (P2)
`repair --strategy` prints a `SearchArtifact` (schema
`concir-repair-artifact-v1`) containing the input program, frozen contract,
effective config and bounds, source identity (crate version and a binary
fingerprint), all nodes/attempts with patch relationships and **full**
verification reports, the patch chain, accepted program/report, counts, stop
reason, and a reproduce command. `replay` reads the artifact, re-applies every
patch to its parent, validates fingerprints and the input, and re-verifies the
accepted program; broken parents, patch bases, and tampered inputs fail
explicitly. `tests/cli.rs::e8_repair_artifact_round_trips_and_replays` and
`tests/repair_search.rs::e8_artifact_replays_and_rejects_tampering`; `bench
--artifact` writes complete per-case records
(`e8_bench_writes_complete_records`).

## 3. Architecture summary

- `src/repair/search.rs`: the three strategies (A single, B composite, C
  diagnostic) share one edit space, permissions, verification semantics, and
  budgets. Each node stores its parent, depth, total edits, program, program
  fingerprint, incoming patch, and full report. Dedup, budgets, counters,
  stop reasons, and the artifact/replay live here.
- `src/repair/candidates.rs`: `RepairContext` carries the node's program,
  frozen `ContractSpec`, depth, structured `VerificationReport`, and ancestor
  history; `relevant_resources()` reads `blocked[*].resource_name` facts.
- `src/repair/benchmark.rs`: the development benchmark and its independent
  expectations, with complete artifacts per record.
- `src/bin/concir-backend.rs`: `repair --strategy ... [--artifact]`, `replay`,
  `bench [--artifact]`, and the legacy positional repair path.

## 4. Development benchmark (updated with dedup)

Records via `concir-backend bench`. Columns: proposals / unique programs /
verification calls / cache hits / cumulative states.

| Case | Expected | A Single | B Composite | C Diagnostic |
| ---- | -------- | -------- | ----------- | ------------ |
| already_correct | already_satisfied | 0/1/1/0 | 0/1/1/0 | 0/1/1/0 |
| single_cycle | repaired 1 | 1/2/2/0 | 1/2/2/0 | 1/2/2/0 |
| two_cycles | repaired 2 | 4/5/5/0 ✗ | 7/7/7/1 ✓ | 5/6/6/0 ✓ |
| cross_module_two_cycles | repaired 2 | 4/5/5/0 ✗ | 7/7/7/1 ✓ | 5/6/6/0 ✓ |
| forbidden_scope | no_acceptable | 0/1/1/0 | 0/1/1/0 | 0/1/1/0 |
| preserved_unfixable | no_acceptable | 2/3/3/0 | 8/4/4/5 | 8/4/4/5 |
| no_lock_candidate | no_acceptable | 0/1/1/0 | 0/1/1/0 | 0/1/1/0 |
| budget_truncated | budget_exhausted | 1/2/2/0 | 1/2/2/0 | 1/2/2/0 |

The composite capability (B vs A) and the diagnostic guidance (C vs B: 5
proposals / 6 verifications vs 7/7 for the same two-edit repair) are separately
observable. `preserved_unfixable` shows the dedup effect: 8 proposals but only
4 verifications and 5 cache hits.

## 5. Export and replay

```bash
# Produce a repair artifact.
concir-backend repair tests/repro_bench/two_cycles.json \
    tests/repro_bench/two_cycles_contract.json --strategy c --artifact /tmp/art.json

# Independent replay: rebuild every node, validate fingerprints, re-verify.
concir-backend replay /tmp/art.json

# Development benchmark with complete records.
concir-backend bench --artifact /tmp/bench.json
```

`replay` does not depend on the producer process or the original working
directory: the artifact embeds the input program and frozen contract.
`tests/repair_search.rs::e8_artifact_replays_and_rejects_tampering` mutates a
patch base hash and a parent reference and asserts explicit failure.

## 6. Commands and results

```bash
INSTA_UPDATE=no cargo test --offline --all-targets --no-fail-fast
# 222 passed / 0 failed

cargo test --test ast_roundtrip      # 2 passed
cargo test --test repair_search      # 16 passed
cargo test --test benchmark          # 4 passed
cargo test --test cli                # 4 passed
cargo test --test dot_export         # 19 passed
```

No warnings. No unrelated build artifacts are committed.

## 7. Remaining limits

1. The edit space is still adjacent, side-effect-free `mutex_lock` swaps. No
   other operation category is enabled.
2. Diagnostic guidance uses blocked-resource relevance only; it does not yet
   consume cycle structure, CIR source locations, or per-node property diffs.
3. Search is BFS with fingerprint dedup and fixed budgets; no cost model beyond
   fewest edits, and no optimality claim.
4. Unknown/invalid/unsupported intermediate nodes are not expanded, so a fix
   that must pass through a temporarily `UNKNOWN` program is not found.
5. Unstructured `spawn` without `join` still grows and yields `UNKNOWN`.
6. The `Unsupported` surface is unchanged (`RwLock`, `select`, async, holes,
   async condvar, float control, channel close).
7. Fairness is not modeled; `AG EF` remains reachability preservation.
8. The benchmark is a development set; it must not be used to compute
   generalisation or as the paper's evaluation set.
