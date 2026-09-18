# PILOT_HANDOFF

Phase: non-LLM experimental **pre-run tooling + small pilot**. This document
reports what was actually executed, not a plan, and not a formal paper result.

Baseline under test: `e8480c4a54840e68d7cf3a62c5075e4fc02e6966`
(`229 passed / 0 failed / 0 warnings`). The core `src/` was not modified this
round; the working tree is dirty only with the new untracked `scripts/` and
`experiments/` files (see `git.status_porcelain` below).

## 1. What was built

| Artifact | Path | Purpose |
| --- | --- | --- |
| Case generator | `experiments/pilot-v1/generate_cases.py` | emits 17 deterministic pilot cases + contracts + witness programs + `manifest.json` |
| Runner | `scripts/run_pilot.py` | manifest-driven A/B/C batch with timeout, replay, resume, JSONL/CSV, selftest |
| Raw inputs | `experiments/pilot-v1/cases/` | 17 models + 17 frozen contracts |
| Witnesses | `experiments/pilot-v1/witness/` | one fixed program + enlarged-bound contract per case |
| Raw results | `experiments/pilot-v1/results.jsonl` | one record per run (228) |
| Summary | `experiments/pilot-v1/summary.csv`, `summary.json` | flat table + paired/determinism analysis |
| Raw process I/O | `experiments/pilot-v1/raw/<suite>/<case>__<config>__<strategy>__r<repeat>/` | `search.stdout`, `search.stderr`, `search.exit`, `artifact.json`, `replay.*` |
| Build/环境 | `experiments/pilot-v1/environment.json`, `run_meta.json` | commit, dirty state, binary sha256, toolchain, machine, command/config ids |
| Witness check | `experiments/pilot-v1/witness_results.json` | both engines on every witness |
| Selftest | `experiments/pilot-v1/selftest/` | runner behaviour checks |

The runner uses one pre-built release binary and drives the existing CLI only;
no core refactor. Every complete artifact is replayed by a **separate** CLI
call, and replay time is stored apart from search CLI wall time.

## 2. Environment and identity

```
git commit          e8480c4a54840e68d7cf3a62c5075e4fc02e6966
git status          "?? experiments/"  "?? scripts/"
git diff HEAD sha   "" (core source unchanged; dirty state is untracked files)
binary              target/release/concir-backend
binary sha256       65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6
rustc               1.100.0-nightly (a69a63265 2026-09-03)
cargo               1.100.0-nightly (b2e9d5f9d 2026-09-02)
python              3.14.0
machine             macOS-26.6.2 arm64 (12 cpus)
search config ids   smoke|main {candidate=64,verification=64,depth=4,edits=4}
                    tight       {candidate=8, verification=4,  depth=1,edits=1}
per-process timeout 30 s       total run budget 600 s (not reached)
```

The package version is **not** used as the source identity; the commit plus the
tracked-diff hash plus the binary sha256 are. The two dirty files are untracked
experiment scaffolding only.

## 3. Commands actually run

```sh
cargo build --release --offline                                  # build once (not counted as run time)
python3 experiments/pilot-v1/generate_cases.py                    # regenerate cases/manifest
python3 scripts/run_pilot.py validate-witness                     # 17/17 match expected on petri + interp
python3 scripts/run_pilot.py run --suite smoke --suite pilot \
        --timeout 30 --total-budget 600                           # 228 runs
python3 scripts/run_pilot.py summarize
python3 scripts/run_pilot.py selftest                             # 5/5 PASS
```

The full search log is reproducible from `results.jsonl` and the per-run
directories; the exact argv is stored in each `search.command`.

## 4. Case data (pilot is generated, not held-out)

17 pilot cases from a documented template (see the generator header). Each unit
is a pair of functions acquiring two mutexes in opposite order; an interference
pair is a benign, reachable, consistently ordered lock pair on unrelated
resources. Family summary:

| family | cases | construction |
| --- | --- | --- |
| `one_defect` | p1_same, p1_cross | 1 ABBA pair, same module / across modules |
| `two_defects` | p2_same, p2_cross | 2 independent ABBA pairs |
| `three_defects` | p3_same, p3_cross | 3 independent ABBA pairs |
| `interference` | p1_interf, p2_interf, p3_interf | defects + reachable benign lock pairs |
| `enumerate_order` | p2_names_swapped, p2_modules_swapped | same as p2 with worker/module order permuted |
| `controls` | c_already_correct, c_already_correct_interf, c_scope_restricted, c_lock_reorder_forbidden, c_bounds_unknown, c_preserved_unsatisfiable | correct / out-of-scope / too-small-bounds / preserved unsatisfiable |

(17 pilot cases total.)

**Independent expectation** (never derived from A/B/C results):

* A **witness** program (every defect pair unified to one lock order) is
  generated per case and checked against the frozen contract semantics with the
  **Petri and interpreter engines independently**; for the normal cases the
  outcome must be `PASS`, and for the preserved-unsatisfiable control it must be
  `FAIL`. `17/17` match the expected outcome on both engines. Both engines share
  the lowerer/interpreter infrastructure, so this is a cross-check, not a formal
  proof. For the deliberately-bounded cases (`p3_*`, `c_bounds_unknown`) the
  witness is checked under an enlarged analysis bound (`max_states=1_000_000`)
  so the pilot bound cannot hide a valid witness.
* The minimum patch length is stated as an **argument** from disjoint pairs
  (`k` units ⇒ at least `k` adjacent swaps), not measured by a search.
* `defect_constructed`, `already_correct`, `fix_out_of_scope`, `bounded_analysis`
  flags live in `manifest.json` (`independent_expectation`).

Analysis bounds are frozen in each contract: 200 000 states for p1/p2/controls;
20 000 for the three-defect cases (`p3_*`); 50 for `c_bounds_unknown`. Search
budgets are shared by A/B/C; only `strategy` changes. A uses the same public
budgets with its single-step limit (it never expands a child).

## 5. Smoke (8 dev cases × 3 strategies)

These are only smoke/regression, not pilot or paper data.

| case | A | B | C |
| --- | --- | --- | --- |
| already_correct | already_satisfied v1 | already_satisfied v1 | already_satisfied v1 |
| single_cycle | repaired v2 | repaired v2 | repaired v2 |
| two_cycles | no_acceptable v5 | **repaired v7** | **repaired v6** |
| cross_module_two_cycles | no_acceptable v5 | **repaired v7** | **repaired v6** |
| forbidden_scope | no_acceptable v1 | no_acceptable v1 | no_acceptable v1 |
| preserved_unfixable | no_acceptable v3 | no_acceptable v4 | no_acceptable v4 |
| no_lock_candidate | no_acceptable v1 | no_acceptable v1 | no_acceptable v1 |
| budget_truncated (cand=1) | budget_exhausted v2 | budget_exhausted v2 | budget_exhausted v2 |

All 24 smoke artifacts replayed (exit 0); classifications are identical to the
already-reviewed development benchmark costs (two_cycles B=7/C=6).

## 6. Pilot results (main config, repeat r1; 17 cases × 3)

`vN` = verification calls, `sN` = cumulative states.

| case | family | A | B | C |
| --- | --- | --- | --- | --- |
| p1_same | one_defect | repaired v2 s92 | repaired v2 s92 | repaired v2 s92 |
| p1_cross | one_defect | repaired v2 s92 | repaired v2 s92 | repaired v2 s92 |
| p1_interf | interference | repaired v2 s3172 | repaired v2 s3172 | repaired v2 s3172 |
| p2_same | two_defects | no_acceptable v5 s9927 | repaired v7 s13445 | repaired v6 s11610 |
| p2_cross | two_defects | no_acceptable v5 s9927 | repaired v7 s13445 | repaired v6 s11610 |
| p2_names_swapped | enumerate_order | no_acceptable v5 s9927 | repaired v7 s13445 | repaired v6 s11610 |
| p2_modules_swapped | enumerate_order | no_acceptable v5 s9927 | repaired v7 s13445 | repaired v6 s11610 |
| p2_interf | interference | no_acceptable v6 s72768 | repaired v8 s93856 | repaired v6 s69600 |
| p3_same | three_defects | analysis_unknown v1 s20001 | analysis_unknown | analysis_unknown |
| p3_cross | three_defects | analysis_unknown v1 s20001 | analysis_unknown | analysis_unknown |
| p3_interf | interference | analysis_unknown v1 s20000 | analysis_unknown | analysis_unknown |
| c_already_correct | controls | already_satisfied v1 s8 | already_satisfied | already_satisfied |
| c_already_correct_interf | controls | already_satisfied v1 s38 | already_satisfied | already_satisfied |
| c_scope_restricted | controls | no_acceptable v1 s49 | no_acceptable | no_acceptable |
| c_lock_reorder_forbidden | controls | no_acceptable v1 s49 | no_acceptable | no_acceptable |
| c_bounds_unknown | controls | analysis_unknown v1 s50 | analysis_unknown | analysis_unknown |
| c_preserved_unsatisfiable | controls | no_acceptable v3 s135 | no_acceptable v4 s176 | no_acceptable v4 s176 |

Tight config (`candidate=8, verification=4, depth=1, edits=1`): p1 cases still
repaired v2; **all five two-defect cases become `budget_exhausted` with
`stop_reason=verification-budget` and v4** for A/B/C; the
preserved-unsatisfiable control becomes `budget_exhausted`/`max-depth` for B/C
(they would expand a depth-1 FAIL child) but stays `no_acceptable` for A (its
single-step limit); three-defect cases stay `analysis_unknown`; the other
controls are unchanged. So the tighter search budget binds before depth
truncation on this family.

### Final classification totals (all 228 runs)

| suite | repaired | already_satisfied | no_acceptable | budget_exhausted | analysis_unknown |
| --- | ---: | ---: | ---: | ---: | ---: |
| smoke (24) | 7 | 3 | 11 | 3 | 0 |
| pilot (204) | 66 | 24 | 49 | 17 | 48 |
| total (228) | 73 | 27 | 60 | 20 | 48 |

No `external_timeout`, `resource_error`, `json_error`, `artifact_missing`,
`replay_failed`, or `replay_timeout` occurred. All 228 artifacts replayed
(exit 0), max search wall 3 140 ms (p2_interf B), sum search 70.9 s, sum replay
107.6 s. (`c_preserved_unsatisfiable` is the `no_acceptable` control where the
lock fix is rejected because the preserved goal is unreachable for every edit.)

## 7. Paired A/B/C analysis

**Q1 — does B give stable composite repair for multiple defects?**
On the five two-defect cases (same/cross/order-permuted/interference), A never
repairs (`no_acceptable`, its documented single-step limit), while B repairs
every one under the main budget with patch length 2. On single-defect cases A,
B and C are identical. On the three-defect cases every strategy stops at the
frozen analysis bound (`analysis_unknown`); that is a state-space limit, not an
A/B difference, and is retained, not masked.

**Q2 — when does C's diagnostic filtering reduce verification?**
Over the eight `main` cases that both B and C repair (r1):

| metric | B | C |
| --- | ---: | ---: |
| verification calls (sum) | 42 | **36** |
| states explored (sum) | 150 992 | **119 396** |
| CLI wall (sum, ms) | 4 539 | **3 544** |

C ≤ B verification calls on 8/8 and strictly fewer on 5/8 (all two-defect
cases: 7→6; p2_interf 8→6; single-defect cases tie at 2). **C never missed a
repair B found in this pilot.** The gains are modest and come from skipping
benign/unrelated candidates; the absolute cost is still dominated by verifying
the (large) state space of each surviving candidate. No case showed C worse than
B, but the sample is small and generated from one template.

**Q3 — verification state space vs patch search.**
Scaling the construction moves the bottleneck:

* 1 defect: root 49–1 694 states; repair is cheap and identical across A/B/C.
* 2 defects: root ~2 211 states, ~99 27 total with A; B/C patch search costs
  6–8 verifications. Adding one benign reachable pair grows root to 13 256 and
  total to ~94 k states (~3 s), the most expensive pilot run.
* 3 defects: root alone needs ~103 825 states (measured at an enlarged bound);
  at the frozen 20 000 bound all strategies return `analysis_unknown` before any
  patch search. A tight search budget on 2-defect cases hits
  `verification-budget` at v4.

So for this family the limit is **verification state space first and patch
search second**: patch search is the binding cost at 1–2 defects, state space at
3 defects and with extra concurrency.

## 8. Determinism and process behaviour

* Three `main` repeats for every pilot case/strategy: **0 nondeterministic
  groups** on all non-time fields (`outcome, stop_reason, truncation,
  saw_unknown, proposals, unique_programs, verification_calls, cache_hits,
  states_explored, nodes, patch_len, replay*`).
* Wall-time spread is small (≤22 ms on sub-second cases, ≤21 ms on the ~3 s
  p2_interf case). Wall time includes process start and JSON serialisation and
  is **not** pure model-checking time.
* Runner selftest (5/5): legal artifact on non-zero exit; normal repaired
  output; external timeout kill (exit −9, partial kept); corrupted artifact and
  corrupt JSON both rejected by `replay`; resume fingerprint mismatch detected.
* Resume: replaying the pilot with `--resume` reused **204/204** records with 0
  mismatches (verified against a copy under `/tmp/resume-test`).

## 9. Limits / what this is not

* Pilot cases are **generated from one template** (mutex ABBA pairs). They are
  not held-out, not real-program-derived, and not suitable for a paper claim.
* Witness/Petri/interpreter checks share lowering infrastructure; the
  dual-engine agreement is a cross-check, not a proof.
* Minimum patch lengths are arguments, not exhaustive minima.
* The three-defect bound is deliberately small, so its `UNKNOWN` is a boundary
  report, not evidence about repair capability at 3 defects.
* One machine, one binary, no statistical significance testing; timing is
  wall-clock with process overhead.
* Existing 8 dev cases remain smoke/regression only.

## 10. Next-step recommendations (from the measured data)

* **Informative scale parameters:** 2 defect units with 1 reachable interference
  pair is the informative point (nonzero diagnostic effect, tractable); 3 units
  are informative only as a state-space boundary until analysis improves.
* **Families needing real independent cases:** non-lock defects (channel
  rendezvous, condition variables), real lock hierarchies with several
  resources per thread, preserved-property conflicts, and edit-scope
  restrictions drawn from real APIs.
* **Performance work:** the measured ceiling is verification state space for
  ≥3 concurrent defect threads. A separate performance/optimization task (state
  dedup, symmetry, or POR) is justified, but is explicitly out of scope for this
  round and must not change repair semantics.
* **Formal case-set collection spec (future, not done here):** record source
  program and commit, a reviewed real-program→CIR mapping, the semantic features
  covered, an independently reviewed property/preservation contract with a
  witness, and split cases by source/family to avoid template leakage.
