# LIFECYCLE_HANDOFF

Scope: runner lifecycle close-out only — immutable batches, persistent attempt
identity, complete `replay_pending` recovery, external `--out`, statistics
identity/missing-evidence checks, and real-path lifecycle regressions. No core
semantics were changed; no LLM/POR/candidate changes; no synthetic case
expansion; the four frozen pilot-v2 batches and pilot-v1 were not modified.

Core: commit `e8480c4`, `INSTA_UPDATE=no cargo test --offline --all-targets
--no-fail-fast` → **229 passed / 0 failed / 0 warnings**.

All this round's outputs live under `experiments/pilot-v2-lifecycle/` and
`scripts/` (plus `examples/pilot_tool.rs`, unchanged from the previous round).

## 1. Fixes

### R1 — immutable batch, persistent attempt identity (`scripts/run_pilot.py`)
* `do_run` refuses to touch an existing batch tag without `--resume`, returning
  **exit 2 before writing environment/plan/snapshot/index** (`do_run`, lifecycle
  gate).
* `cohort_identity` / `cohort_fingerprint` freeze the experiment identity:
  manifest sha256 + identity key, generator sha256, experiment-code fingerprint,
  binary sha256, tool sha256, search/replay timeouts, suites, and the sorted
  plan run-keys/run-fingerprints. `--resume` compares this **before writing**
  and refuses (`exit 2`, `cohort-mismatch` with a field diff) if inputs, configs,
  timeouts, engine, binary or code differ. A new batch tag is required.
* **Rule for `total_budget`:** it is a *per-invocation recovery quota*, not part
  of the cohort identity. Each invocation records its budget in
  `resume_events.jsonl`; the rule is written into `batch.json`
  (`total_budget_rule`).
* `_new_attempt` creates the attempt directory with `exist_ok=False` and a
  cross-invocation-unique id (`utcstamp-pid-attempt_seq`); the raw path includes
  the `run_key` (`raw/<slug>__<run_key>/attempt-<id>`). Old attempts — including
  directories left by a crash — are never reused or overwritten. `attempt_seq`
  continues from the existing `attempts.jsonl` (`next_attempt_seq`).
* On `--resume` the frozen `environment.json`, `plan.json`, `batch.json` and
  `code_snapshot/` are left untouched; only `attempts.jsonl`,
  `valid_index.jsonl` and `resume_events.jsonl` are appended/rewritten.
* `load_resume_index` only reads the **same batch's** index; evidence from a
  different cohort is never mixed. Pure reuse adds no attempt and no sample.

### R2 — complete `replay_pending` recovery
* `_replay_phase` now inherits the original search record
  (`spec["_pending_record"]`) and re-validates it with
  `_verify_pending_artifact`: artifact exists, `artifact_sha256` present and
  matching, schema, outcome equals the pending `raw_outcome`, search exit code
  equals the outcome mapping, embedded `input_program`/`frozen_contract` match the
  run after backend normalization, and `effective_config` matches.
* The recovered record keeps `artifact_path`/hash, `exit_code`,
  `search_wall_ms`, all search counts, `patch_len`, `root_outcome`, config/input
  identity, and adds the replay attempt's command/log/exit/wall. `wall_ms =
  search_wall_ms + replay_wall_ms` (`wall_ms_basis="search+replay"`); a missing
  search cost is never zero and replay time is never labelled search time. Only
  replay runs — never a hidden search.
* `required_complete_fields` / `missing_complete_fields` define the complete
  evidence set; `_commit`, `pilot_analyze` and `pilot_audit` all enforce it.
  Incomplete/tampered artifacts, failed/timeout replay and missing evidence are
  not `complete`. A recovered `complete` record can be resumed again and is
  accepted by summarize and audit.

### R3 — external `--out`, statistics identity, missing-evidence checks
* `identity_key`/`code_identity` no longer call `relative_to(REPO)` blindly:
  files outside the repository get an `external:<absolute-path>` key, so
  `python3 scripts/run_pilot.py --out <tmp> behavior` works and no two files
  collapse by basename.
* `pilot_analyze` groups determinism by the full identity **without repeat**
  (suite/stage/case/config values/strategy/engine/identity/timeouts), and pairs
  B/C by the same identity **without strategy but with repeat**. Different config
  values under one `config_name` are different groups; different code/binary
  identities cannot be paired.
* `summarize_batch` validates the valid index against the frozen `plan.json`
  (same run keys, unique keys, no duplicate repeat inside one identity) and
  reports an **error** for a `complete` record missing any required field instead
  of `or 0`. `pilot_audit` enforces the same completeness.
* `classify_explore` records the backend `report_complete` separately from
  `evidence_status`; the matrix table keeps both. `p3_same/100000` is
  `FAIL, evidence_status=complete, report_complete=False`.

## 2. Tests and results

| check | result |
| --- | --- |
| `behavior` (11 checks, external-safe) | **11/11 PASS** |
| `lifecycle` (8 real-path scenarios) | **8/8 PASS** |
| `scripts/pilot_checks.py` | **2/2 PASS** (valid index only; missing field flagged) |
| `INSTA_UPDATE=no cargo test --offline --all-targets --no-fail-fast` | **229/0/0** |

`lifecycle` scenarios (real `do_run`/`execute_attempt`/`summarize_batch`/audit,
fault injection only at the subprocess boundary):

1. `refuse_existing_batch_without_resume` — exit 2, old tree hash unchanged.
2. `resume_after_crash_no_overwrite` — injected crash after the first run leaves
   one orphan attempt dir; `--resume` finishes to 3/3 complete, `raw/` files
   unchanged, new attempt dirs allocated.
3. `cohort_change_refused_new_tag_ok` — same tag with changed config and changed
   timeout both exit 2 and do not modify the frozen batch; a new tag runs.
4. `failure_not_bound_to_old_artifact` — a current `exit=2` run with no artifact
   is recorded as `no_artifact`; the index keeps the prior complete evidence with
   its artifact bytes/hash unchanged.
5. `replay_pending_recovery_full_and_idempotent` — a valid search-without-replay
   state recovers with all fields preserved, passes summarize and audit; a third
   `--resume` reuses it without a new attempt or re-search.
6. `recovery_refuses_incomplete` — missing artifact and hash mismatch →
   `identity_mismatch`; replay timeout → `replay_timeout`; replay non-zero →
   `replay_failed`; none marked `complete`.
7. `stats_distinct_config_and_pairing` — distinct configs under one name are not
   repeats; a B without a matching C forms no pair.
8. `external_out_behavior` — `--out <tmp> behavior` exits 0.

The whole `lifecycle` command was also run with an OS temporary `--out`
(`mktemp -d /tmp/concir-life-XXXXXX`) and passed 8/8, confirming the external
output path end to end.

## 3. New batch identities and evidence

New batches (immutable; frozen `cohort_fingerprint`, `code_fingerprint`,
binary `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`):

| batch | planned/records | cohort_fingerprint (prefix) | evidence |
| --- | --- | --- | --- |
| `smoke-lifecycle` | 24/24 complete | `d3f090b3bd06ea75` | 24 attempt dirs / 24 attempt rows; re-run without resume refused (exit 2, tree unchanged); `--resume` reused 24, added 0 attempts |
| `matrix-lifecycle` | 12/12 complete (explore) | `008e2d171fc34e23` | `report_complete` column distinguishes FAIL-complete from FAIL-incomplete |
| `lifecycle-<pid>/b1,b2,b3,b5` | 3 records each | `50a0eb90f615`, `bbe4b4b08349` | crash/resume/replay/pending demos; `b2` has 4 attempt dirs (1 orphan) but 3 valid records |

Raw data: `experiments/pilot-v2-lifecycle/results/batches/` and
`experiments/pilot-v2-lifecycle/lifecycle-<pid>/` (`attempts.jsonl`, `raw/…/attempt-<id>/`,
`plan.json`, `batch.json`, `resume_events.jsonl`, `valid_index.jsonl`,
`summary.*`, `tables.md`, `code_snapshot/`).
Independent audit `scripts/pilot_audit.py experiments/pilot-v2-lifecycle` →
**0 issues** (24 complete artifacts re-hashed, counts/exit/replay consistent,
unique run keys, no incomplete complete records).

Commands:

```sh
cargo build --release --offline --example pilot_tool
python3 scripts/run_pilot.py --out experiments/pilot-v2-lifecycle behavior
python3 scripts/run_pilot.py --out experiments/pilot-v2-lifecycle lifecycle
python3 scripts/pilot_checks.py
python3 scripts/run_pilot.py --out experiments/pilot-v2-lifecycle \
  --manifest experiments/pilot-v2/manifest.json run --suite smoke \
  --search-timeout 30 --replay-timeout 30 --total-budget 300 --batch-tag smoke-lifecycle
python3 scripts/run_pilot.py --out experiments/pilot-v2-lifecycle \
  --manifest experiments/pilot-v2/manifest.json run --suite matrix \
  --search-timeout 60 --replay-timeout 30 --total-budget 300 --batch-tag matrix-lifecycle
python3 scripts/run_pilot.py --out experiments/pilot-v2-lifecycle summarize --batch smoke-lifecycle
python3 scripts/pilot_audit.py experiments/pilot-v2-lifecycle
INSTA_UPDATE=no cargo test --offline --all-targets --no-fail-fast
```

## 4. Old data unchanged

`experiments/pilot-v2-lifecycle/frozen-data-sha256.txt` records 3849 file
hashes: 1906 for `experiments/pilot-v1/` (matches the review's
`v1-preservation.json` count) and 1943 for `experiments/pilot-v2/results/`
(the four frozen batches). Manifest digest
`807f9c5305fd3ff8495262d27aaa8037b580bcfacd9ce7df4329aac6a5b9ce85`; re-hashing
after this round is byte-identical. No command in this round wrote to those
trees.

Delivered code hashes (this revision):

```
scripts/run_pilot.py                    23de1cec77bda62d93d1c895fd81204f63de55474a28122f79e6155d39f28421
scripts/pilot_analyze.py                dd017bb3847b2ee04435e82df07dbb0b47a35fcfda91c45999694e019c3363fa
scripts/pilot_audit.py                  e03028f3d9a72fe9ca0068789ef2f3af7beaf0eccd48628f399f66bd09dd1d92
scripts/pilot_checks.py                 2825993457df2ab312d5f6ab6551bb760a47e0bbfa516fd33134446daecb7a70
experiments/pilot-v2/generate_cases.py  e70b688f1e89deb85cf62c965ea6501c2254e3ee5d16de90793afc8ab273e2da
experiments/pilot-v2/manifest.json      1ee30b03805d7d3f4beebb2a38d3429411c6ae163b1ea51dd984d5be4d7020df
examples/pilot_tool.rs                  fb40d6c0ab1580d6ec5270ca11f2fe78df82c66f2667fe2d6a56fe017eca3098
```

## 5. Documentation口径 corrections

Appended to `experiments/pilot-v2/PILOT_V2_HANDOFF.md` section 5 (doc only; the
frozen `results/` data is untouched):

* The main pilot is **16 cases / 180 runs** (14 lightweight cases at main×3 +
  tight×1, and 2 three-defect cases at main×1 + tight×1), not 15 cases.
* The 30 both-repaired B/C pairs contain repeats and two search configs of the
  same models; they are **paired measurements, not 30 independent programs**.
* The heavy `p3_same` B28/C20 result is a **single-case single run**.
* `FAIL` / `search_timeout` / `UNKNOWN` are not complete exploration; the
  matrix `p3_same/100000` (`FAIL`) is bounded-incomplete
  (`report_complete=false`).

## 6. Unfinished / not done (by design)

* The 180-run pilot and the 150 s heavy batch were **not** re-run; only the 24
  smoke and 12 matrix explore runs were repeated under the new runner.
* The four frozen pilot-v2 batches were not re-summarized with the stricter
  analyzer (their records predate `report_complete` and `wall_ms_basis`); they
  remain as delivered. The new analyzer's extra validation was demonstrated on
  the new `smoke-lifecycle`/`matrix-lifecycle` batches instead.
* The binary/code-identity mismatch path is exercised through manifest contents,
  timeout changes and the code fingerprint; it was not tested by swapping in a
  differently-hashed binary file (the cohort comparison code is shared).
* The crash injection is a `KeyboardInterrupt` raised at the subprocess
  boundary; a real OS kill of the Python process was not performed.
* Subsequent substantive work is the traceable real concurrent case corpus and
  source→CIR mapping; this generated pilot still cannot substitute for it.
