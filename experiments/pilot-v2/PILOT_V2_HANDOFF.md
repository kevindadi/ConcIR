# PILOT_V2_HANDOFF

Phase: runner reliability (R1–R5), pilot input/witness corrections (R6), and a
unified-budget pilot-v2 run. This is a **generated development pre-experiment**,
not a held-out evaluation, not a statistical claim, and not a completed paper
experiment. It does not touch the Petri/interpreter/CIR/repair core, which stays
at `e8480c4` (`229 passed / 0 failed / 0 warnings`).

Pilot-v1 data is preserved unchanged under `experiments/pilot-v1/`; this round
writes only under `experiments/pilot-v2/` and `scripts/` (plus the new
`examples/pilot_tool.rs` helper).

## 1. R1–R6 corrections

### R1 — independent attempt, bound evidence (`scripts/run_pilot.py`)
Every execution writes to a fresh `.../attempt-NNN/` directory (`execute_attempt`,
`scripts/run_pilot.py:593`) and the record is bound to the artifact it produced.
`classify_repair` (`:425`) requires: artifact file present, valid JSON object,
schema `concir-repair-artifact-v1`, outcome in the known set, `exit_code` equal
to the outcome's exit mapping, `effective_config` equal to this run's config, and
the embedded `input_program`/`frozen_contract` equal to this run's inputs after
the backend's own serde normalization. `repaired`/`already_satisfied` additionally
require a successful replay. A stale artifact can no longer be read: a current
failure that produced no file is `no_artifact`, never `repaired`.

Input identity uses both the raw byte sha256 and a canonical form produced by
`examples/pilot_tool.rs normalize`, i.e. the exact serializer the artifact
embeds (byte hash and canonical JSON are kept distinct).

### R2 — batch/run/attempt/repeat identity (`:283`, `:374`, `:868`, `:1095`)
`plan_runs` yields one *logical run* per (stage, suite, case, config, strategy,
repeat). Attempts append to `attempts.jsonl`; the summary reads only
`valid_index.jsonl` (`_commit`, `:771`), one chosen record per unique `run_key`.
`run_key` includes the full config content, so a config value change under the
same name is a different run; `run_fingerprint` additionally includes inputs,
timeouts, binary sha256 and the experiment-code fingerprint. Statistics, pairing
and determinism (`scripts/pilot_analyze.py`) read the valid index only.

### R3 — resume only evidence-complete (`:787`, `:805`)
`_record_evidence_ok` requires the fingerprint and stage timeouts to match, the
status to be `complete` or `replay_pending`, the referenced artifact to exist and
its sha256 to equal the stored hash, and `replay_ok` for complete records.
Missing/corrupt artifacts and `replay_timeout` are not reused. A search that
finished but whose replay did not is `replay_pending` and can be completed by an
explicit replay-only recovery, not faked.

### R4 — monotonic deadline (`:868`, `:923`)
`deadline = time.monotonic() + total_budget`; each stage gets
`min(stage_timeout, remaining)`, the remaining budget is re-checked before
replay, and the process group is killed on timeout. Environment/code collection
is measured separately and recorded as `env_outside_budget: true`.

### R5 — experiment code in the reproducible identity (`:139`)
`code_identity` hashes the runner, statistics script, generator, manifest and the
witness tool, and each batch snapshots them under `code_snapshot/`. The run
identity is `git commit` + tracked-diff hash + `code_fingerprint` + binary
sha256; the package version is not used. Outputs are excluded, so generating
results does not change the identity.

### R6 — corrected inputs and witnesses
`experiments/pilot-v2/generate_cases.py`:
* `p1_cross_shared` / `p2_cross_shared` are real cross-module contention: the two
  functions of each defect live in different modules and lock the same mutexes
  through `requires.resources` FQN bindings. `p2_cross_independent` is the
  distinct "one local deadlock per module" case. Structural assertions
  (`assert_structure`) fail generation if cross/name-order/module-order/
  interference do not change the intended dimension.
* A legal witness is produced by one allowed adjacent `mutex_lock` swap per
  defect on the original program (stable sids, all unlocks preserved). The new
  `examples/pilot_tool.rs witness` applies the swaps, records the real
  `AppliedEdit` chain (original function hash, parent/child program fingerprints,
  permission check) and verifies the result against the **original frozen
  contract**.
* Enlarged-bound analysis is a separate `*_expanded_contract` applied to the
  witness program; the original contract is never rewritten. Out-of-scope
  controls store an `auxiliary` semantic model, explicitly not a legal witness,
  and the preserved-unsatisfiable control has no witness at all.
* `constructed_defect_units`, `edit_lower_bound`, `allowed_edit_space_fixable`
  and `minimal_patch_len` are separate manifest fields; controls with no
  acceptable repair have `minimal_patch_len: null`.

### Behavior regression (`scripts/run_pilot.py behavior`)
Real `classify`/`execute_attempt`/`do_run`/`summarize` paths with fault injection
at the subprocess/file boundary; **11/11 PASS**:

| check | result |
| --- | --- |
| normal repair + replay | PASS (`repaired`, replay 0) |
| non-zero exit with legal artifact | PASS (`no_acceptable_candidate`, exit 1, replay 0) |
| stale historical artifact not reused | PASS (`no_artifact`) |
| stdout without artifact | PASS (`no_artifact`, `replay_ok=null`) |
| corrupt JSON artifact | PASS (`json_error`) |
| process spawn error | PASS (`spawn_error`) |
| replay failure on tampered artifact | PASS (replay non-zero) |
| duplicate rerun / only-B | PASS (one valid index entry) |
| same config name, changed value | PASS (distinct run key + fingerprint) |
| resume refuses missing artifact / changed timeout / replay_timeout | PASS |
| tiny total budget with real sleep subprocess | PASS (deadline clamped, killed, elapsed 0.14 s) |

Statistics path: `python3 scripts/pilot_checks.py` runs the real
`pilot_analyze.summarize_batch` on a synthetic batch whose `attempts.jsonl` holds
two attempts for one run; the summary reports `records=1`, proving statistics
read the unique `valid_index.jsonl` and cannot be inflated by reruns
(`experiments/pilot-v2/summarize_check.txt`, PASS).

## 2. Identity and immutable batches

| batch | planned/executed | code_fingerprint | batch_fingerprint |
| --- | --- | --- | --- |
| `smoke-v2` | 24/24 | `99ad9172a5e0…` | `5e0e0fe0bd5f…` |
| `matrix-v2` | 12/12 | `99ad9172a5e0…` | `19c398db787b…` |
| `pilot-v2` | 180/180 | `99ad9172a5e0…` | `f960603cab8f…` |
| `heavy-v2` | 3/3 | `99ad9172a5e0…` | `577fb2aa5b69…` |

All four batches share the same experiment-code fingerprint. Binary
`sha256=65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`,
core commit `e8480c4`. Delivered code hashes:

```
scripts/run_pilot.py                    f1452f74c51b016a0af5fbd79c34ee1b1be066db298b9e9a0887a7d77d8754b3
scripts/pilot_analyze.py                66088feb8947cd751c69bf2ea656f42c4bff173715549d39821280540cf55386
scripts/pilot_audit.py                  a8e10d9b6418c52cc4694d5c296113210b4b0a494bb77ae9cf7ec781d37ff9c0
scripts/pilot_checks.py                 86d5abac737adf883c523d0d10d82f2e0dae7a704cdfb1a36ae39d2d1137a15e
experiments/pilot-v2/generate_cases.py  e70b688f1e89deb85cf62c965ea6501c2254e3ee5d16de90793afc8ab273e2da
experiments/pilot-v2/manifest.json       1ee30b03805d7d3f4beebb2a38d3429411c6ae163b1ea51dd984d5be4d7020df
examples/pilot_tool.rs                  fb40d6c0ab1580d6ec5270ca11f2fe78df82c66f2667fe2d6a56fe017eca3098
```

Raw data: `experiments/pilot-v2/results/batches/<id>/` with `attempts.jsonl`
(append-only), `valid_index.jsonl` (the unique valid-run index), `raw/.../attempt-NNN/`
(stdout/stderr/exit/artifact/replay/command), `code_snapshot/`, `environment.json`,
`plan.json`, `batch.json`, `summary.csv`, `summary.json`, `tables.md`.
Independent audit: `scripts/pilot_audit.py` → `experiments/pilot-v2/audit.json`,
**0 issues** (all 203 complete artifacts re-hashed and matched, counts/exit
codes/replay consistent, no duplicate run keys).

## 3. Commands run

```sh
cargo build --release --offline
cargo build --release --example pilot_tool --offline
python3 experiments/pilot-v2/generate_cases.py
python3 scripts/run_pilot.py behavior
python3 scripts/run_pilot.py validate-witness --timeout 180
python3 scripts/run_pilot.py run --suite smoke  --search-timeout 30  --replay-timeout 30  --total-budget 300 --batch-tag smoke-v2
python3 scripts/run_pilot.py run --suite matrix --search-timeout 60  --replay-timeout 30  --total-budget 400 --batch-tag matrix-v2
python3 scripts/run_pilot.py run --suite pilot  --search-timeout 30  --replay-timeout 60  --total-budget 600 --batch-tag pilot-v2
python3 scripts/run_pilot.py run --suite heavy  --search-timeout 150 --replay-timeout 180 --total-budget 600 --batch-tag heavy-v2
python3 scripts/run_pilot.py summarize --batch smoke-v2   # and matrix-v2, pilot-v2, heavy-v2
python3 scripts/pilot_audit.py experiments/pilot-v2
INSTA_UPDATE=no cargo test --offline --all-targets --no-fail-fast
```

Every batch finished inside its total budget with `not_executed: 0`. Wall time
includes process start and serialization; it is not pure model-checking time.
Build and environment collection are outside the run budgets and recorded
separately.

## 4. Results (see the script-generated tables)

`experiments/pilot-v2/PILOT_V2_TABLES.md` concatenates the per-batch
`tables.md`; all numbers come from the valid index, none are hand-copied.

* **Smoke (8 dev cases × 3):** unchanged from the reviewed development
  benchmark, e.g. `two_cycles` A `no_acceptable`, B/C `repaired` (B v7, C v6).
* **Root verification matrix** (explore only, no search; `matrix-v2`):

| case | max_states=20000 | 100000 | 200000 |
| --- | --- | --- | --- |
| p1_same | FAIL (49) | FAIL (49) | FAIL (49) |
| p2_same | FAIL (2211) | FAIL (2211) | FAIL (2211) |
| p1_interf | FAIL (284) | FAIL (284) | FAIL (284) |
| p3_same | UNKNOWN (20001) | FAIL (100000) | FAIL (103825, ~4.6 s) |

  This confirms the pilot-v1 confound: the earlier three-defect `UNKNOWN` was an
  artifact of the deliberately smaller 20000 bound, not a property of the model.
* **Pilot (16 cases; unified `max_states=200000`):** 180 records, 176 complete,
  4 `search_timeout` (three-defect main B/C). Classifications:
  `repaired 72`, `already_satisfied 12`, `no_acceptable_candidate 54`,
  `budget_exhausted 26`, `analysis_unknown 12`, `search_timeout 4`.
  * One-defect same/cross-shared/interference all repair under A/B/C with 2
    verifications.
  * Two-defect cases: A `no_acceptable` (single-step), B/C repair (main config).
  * Three-defect (`p3_same`, `p3_cross_independent`): A completes with
    `no_acceptable` (v7, 647 251 states, ~28 s); B/C exceed the 30 s per-process
    budget at the main config; under the tight search budget all three stop at
    `budget_exhausted`/`verification-budget` (v4, 375 538 states, ~16 s).
* **Heavy batch (`p3_same`, main config, 150 s):** A `no_acceptable`
  (v7, 647 251 states, 28 s); **B `repaired` v28 / 2 298 516 states / 99.6 s;
  C `repaired` v20 / 1 664 282 states / 72.1 s.** Complete three-defect repair is
  possible but needs ~70–100 s and millions of verified states.

## 5. Paired B/C and denominators

All-repeat paired summary (`pilot-v2/summary.json`): 60 pairs, 30 both-repaired,
C ≤ B verification on 30/30, C < B on 15/30; on both-repaired pairs sum
verification B/C = 150/132, sum states B/C = 480 602/385 814, sum wall B/C =
14 213/11 318 ms. No pair had a differing success set (`success_differs: []`).

First-repeat (r1) paired rows (script-extracted, in `PILOT_V2_TABLES.md`): 12
both-repaired rows, sum verification B/C = 54/48, C ≤ B 12/12, C < B 5/12. C is
never worse and never misses a B repair in this pilot; the gain is modest and
comes from skipping unrelated candidates. `p2_cross_shared` is the one two-defect
case where B also uses 6 verifications (B = C).

Denominators are explicit per batch (`summary.json`/`tables.md`): successful new
repairs, originally correct (`already_satisfied`), `no_acceptable_candidate`,
`budget_exhausted`, `analysis_unknown`, and external `search_timeout` are never
merged. Repeats are not independent problems: the 180 pilot records are 16 cases
× 3 strategies × (3 main repeats + 1 tight) for the 14 lightweight cases plus the
2 three-defect cases at main×1 + tight×1 (see the manifest `repeat_policy`).
The 30 both-repaired B/C pairs therefore contain repeats and two search configs
of the same models — they are paired measurements, **not 30 independent
programs**. The heavy `p3_same` result (B28/C20) is a single-case single-run
observation. `FAIL`/`search_timeout`/`UNKNOWN` are not complete exploration; in
particular the matrix `p3_same/100000` result is `FAIL` but the bounded
exploration did not finish (see the lifecycle round's `report_complete` column).

## 6. Witnesses

`experiments/pilot-v2/witness_results.json` (generated by `validate-witness`):

* 7 legal witnesses for the satisfiable one/two-defect cases and the two
  order-permuted cases: allowed adjacent swaps, permissions OK, `PASS` under the
  **original frozen contract** (e.g. p2_same chain 2, 1683 states).
* `p3_same` / `p3_cross_independent`: legal chain 3, permissions OK, `PASS` under
  the original 200000 bound (68 923 states).
* `c_bounds_unknown`: legal chain 2 but `UNKNOWN` under the original 50-state
  contract; under the separate expanded contract (1 000 000) the witness is
  `PASS` (1683 states). Both contracts are kept and labelled.
* `c_scope_restricted` / `c_lock_reorder_forbidden`: auxiliary semantic models
  (`PASS` under exploration) that are **not** legal witnesses because the patch
  is outside the contract's edit scope.
* `c_preserved_unsatisfiable`: no witness; the lock fix cannot restore the
  unreachable preserved goal.

## 7. Bottleneck verdict

For this generated family the binding cost is **verification state space**, not
patch search. The matrix separates the two: a three-defect root verifies at
200000 (103 825 states, ~4.6 s) and even at 100000 finds a counterexample, while
the 20000 bound returns `UNKNOWN`. The complete three-defect search enumerates
only 53 proposals but verifies 28 programs for 2.3M cumulative states (B),
which is why it does not fit a 30 s process budget. One/two-defect cases are
patch-search bounded and cheap. No POR/symmetry/candidate optimization was
implemented this round; the data supports evaluating a separate performance task,
but does not by itself prove one is required.

## 8. pilot-v1 correction note (original data unchanged)

The pilot-v1 results and artifacts are preserved as they were; the following
labels/claims were wrong and are corrected in v2:

* **`p1_cross` was not cross-module.** In v1 it contained only module `main` and
  was identical to `p1_same` apart from the program name; the `cross=True`
  parameter did not change the model. v2 uses `p1_cross_shared` (real shared
  locks across `main`/`other`) and `p2_cross_independent` (distinct case).
* **Witnesses were not legal repair patches.** v1 rewrote whole lock functions
  (changing unlock resources and sid bindings) and verified them against a
  widened 1 000 000-state contract renamed per witness. v2 applies allowed
  adjacent swaps (stable sids, all unlocks preserved), records the actual patch
  chain/fingerprints/permission checks, and verifies against the original
  contract, keeping any expanded-bound check as a separate labelled contract.
* **Control `minimum patch length = 1` labels were removed.** Controls with no
  acceptable repair now have `minimal_patch_len: null`; constructed defect
  counts, edit lower bounds and in-scope fixability are separate fields.
* The v1 runner failure modes (stale artifact success, statistics mixing,
  incomplete resume, soft deadline, empty dirty-source hash) are the R1–R5 items
  fixed here; the review found **no** contamination in the 228 v1 records
  themselves, and the v1 fact that 42/36 B/C verifications were reported stands.
* The v1 handoff's hand-written wall-time table (4539/3544 ms) differed from the
  raw JSONL (4482/3523 ms); v2 tables are generated by `pilot_analyze.py`.

## 9. Limits

* Pilot cases are generated from one template (mutex lock-order pairs) and are
  not real-program-derived or held-out. No generalisation claim.
* Witness/interpreter/Petri checks share lowering infrastructure; agreement is a
  cross-check, not a proof.
* `minimal_patch_len` is only stated where a disjoint-pair lower bound is matched
  by a verified witness; otherwise it is null.
* Timings are wall-clock on one machine and include process overhead; no
  statistical significance is claimed.
* The preserved-unsatisfiable and out-of-scope controls are semantic models, not
  legal repair witnesses.

## 10. Next steps

* Informative scale: two defects with one reachable interference pair (nonzero
  diagnostic effect, tractable); three defects are informative as a state-space
  boundary and now measured under a unified bound.
* Real independent cases are still needed for non-lock defects, several
  resources per thread, preserved-property conflicts and edit-scope
  restrictions drawn from real APIs.
* A separate performance task (state dedup / symmetry / POR) is the natural next
  step if this state-space cost is confirmed on real cases; it must not be done
  by changing repair semantics. Do not start it on template-only evidence alone.
