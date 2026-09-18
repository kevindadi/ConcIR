# REAL_CASES_V0_HANDOFF

Scope: (1) three experiment-validity fixes, (2) a sourced real-concurrency case
study v0. Core ConcIR stays at `e8480c4` with no semantic changes and no LLM code;
all this round's artifacts live under `experiments/real-cases-v0/` and `scripts/`.
The earlier pilot data (`experiments/pilot-v1/`, `experiments/pilot-v2/results/`)
is unchanged.

Core check: `INSTA_UPDATE=no cargo test --offline --all-targets --no-fail-fast`
→ **229 passed / 0 failed / 0 warnings**.

## 1. Validity fixes (R1–R3)

### R1 — invalid evidence leaves the success index
* `scripts/run_pilot.py`: `record_evidence_errors(rec)` performs cheap
  evidence/binding checks (artifact exists, sha256 matches, outcome/exit mapping,
  replay flags, required fields) without re-running the backend;
  `_commit` demotes any `complete`/`replay_pending` record whose evidence fails to
  `evidence_invalid` **before** best-record selection, so a stale success can
  never win over a newer failure; `revalidate_index` does the same when a batch
  index is loaded on `--resume`.
* `scripts/pilot_analyze.py`: `evidence_reference_errors` re-checks every
  `complete`/`pending` record during summarize; failing records are reported as
  errors and are not counted as `complete`/success (no `or 0`).
* `scripts/pilot_audit.py`: enforces the same completeness/reference rules.
* Regressions: `scripts/run_pilot.py lifecycle` check
  `invalid_old_evidence_demoted_and_rejected` (delete artifact, resume with a
  failing retry → index demoted, summarize rejects); `scripts/pilot_checks.py`
  `summarize_rejects_invalid_references` (artifact-missing + hash-mismatch).

### R2 — comparable time semantics
* `scripts/run_pilot.py`: `wall_ms` is the **search** cost on every path
  (`wall_ms_basis="search"`), including replay recovery; recovery additionally
  records `replay_wall_ms` and `end_to_end_wall_ms = search_wall_ms +
  replay_wall_ms`. Replay is never recorded as search time.
* `scripts/pilot_analyze.py`: strategy comparison and determinism use
  `search_wall_ms`; tables/CSV report `search_wall_ms`, `replay_wall_ms`,
  `end_to_end_wall_ms`, and `cumulative_attempt_wall_ms` (sum over all attempts
  from `attempts.jsonl`) as separate columns.
* Regression: lifecycle check `recovery_search_cost_comparable` (recovered
  record has `wall_ms == search_wall_ms` and separate `end_to_end_wall_ms`).

### R3 — pairing keeps every outcome
* `scripts/pilot_analyze.py`: B/C pairing is built from the frozen plan and the
  valid index and keeps timeout / UNKNOWN / no_acceptable / not_executed / tool
  failure; it reports `planned_pairs`, `attempted_pairs`, `both_complete`,
  `both_repaired`, `one_side_not_executed`, `one_side_invalid`,
  `success_differs_count` with the differing rows, and a separate
  `both_repaired` cost subset (`search_wall_ms` only).
* Regression: lifecycle check `pairing_keeps_timeout_and_not_executed`
  (B repaired/C timeout, B timeout/C repaired, one not_executed, both repaired →
  planned 4, attempted 4, both_repaired 1, success differences 3, not_executed 1,
  cost subset 1).

All lifecycle checks pass (11/11), `behavior` 11/11, `pilot_checks` 3/3, and the
3849-file frozen pilot checksum (v1 = 1906) is unchanged (see §5).

## 2. Candidate investigation

7 candidates across 6 upstream projects were checked (full table and per-field
provenance in `candidates.json`): `ros2/rmw_zenoh #998`, `xacrimon/dashmap #369`,
`xacrimon/dashmap #195`, `rust-lang/rust #121949`, `rust-lang/rust-analyzer
#22891`, `nnethercote/dhat-rs #25`, `rust-lang/futures-rs #1170`.

* **Included:** `rmw-zenoh-998` (real two-`std::mutex` ABBA, supported) and
  `dashmap-369` (faithful per-shard `RwLock` ABBA, kept as a support boundary).
* **Excluded:** dashmap #195 (unbounded shards + RwLock, no faithful bounded
  model); rust #121949 (OS-level lock, not a program lock-order cycle); dhat-rs
  #25 (allocator reentrancy); futures #1170 (async runtime); rust-analyzer #22891
  (blocking cargo, not a lock order). Reasons are in `candidates.json`.

The upstream-fix model is never used as a repair witness. `rmw-zenoh-998` has a
legal adjacent-swap witness found and verified by the tool; `dashmap-369` is
outside the supported space.

## 3. Results

Independent case count is **2**; total run count is **9** (3 root-verification
records + 6 A/B/C records). Batches: `rc-root` (3/3 complete) and `rc-abc`
(6/6 complete). `scripts/pilot_audit.py experiments/real-cases-v0` → 0 issues.

### Root verification (`rc-root`, `petri`, max_states 20000)

| model | outcome | report_complete | states | model hash (16) |
| --- | --- | --- | --- | --- |
| `rmw-zenoh-998` buggy | **FAIL** | true | 49 | `77f6e181d701d23e` |
| `rmw-zenoh-998` fixed (hypothesis) | **PASS** | true | 39 | — |
| `dashmap-369` buggy | **UNSUPPORTED** | false (no analysis) | 0 | `84f94127f4cd8e3d` |

`rmw-zenoh-998` root `FAIL` matches the upstream gdb 2-cycle. `dashmap-369` is
CIR-syntax valid but the v1 lowerer marks `rwlock` unsupported, so no analysis
runs.

### A/B/C repair (`rc-abc`, `main`: candidate 64/verification 64/depth 4/edits 4)

| case | A | B | C |
| --- | --- | --- | --- |
| `rmw-zenoh-998` | repaired v2 / 92 s | repaired v2 / 92 s | repaired v2 / 92 s |
| `dashmap-369` | unsupported | unsupported | unsupported |

* `rmw-zenoh-998`: a legal witness exists — one allowed adjacent `mutex_lock`
  swap (`main::rx_callback`: `s1`,`s2`), permission check OK, final `PASS` under
  the frozen contract (`cases/rmw-zenoh-998/witness.json`, chain length 1,
  `original_function_hash=6d06d7dcafbcbff4`,
  `program_fingerprint=576a6d40050e7651`). The runner's A/B/C artifacts replay
  (audit clean).
* `dashmap-369`: no candidate is enumerable because there are no `mutex_lock`
  pairs (RwLock is unsupported), so this is not a repair result.

### Defect / verification / repair, separated

| case | defect representable in CIR syntax | verification completes | repaired in current patch space |
| --- | --- | --- | --- |
| `rmw-zenoh-998` | yes | yes (FAIL) | yes (legal 1-swap witness) |
| `dashmap-369` | yes (RwLock syntax) | no (unsupported) | no (outside space) |

## 4. Mapping and reduction limits

Per-case `mapping.md` lists the thread/task correspondence, resource identity,
lock/unlock points, preserved properties and edit scope, plus omitted code and
the assumptions. Key caveats:

* `rmw-zenoh-998` collapses the interval between the two lock acquisitions, so
  the model is more adjacent-swap-friendly than the source. The real fix
  suggested upstream (release `mutex_` before taking `condition_mutex`) is not an
  adjacent lock swap; the tool's witness is a hypothesis of the reduced model,
  not the upstream patch. Condvars, payload/queue logic and thread lifetimes are
  not modelled.
* `dashmap-369` abstracts shard hashing to two named shards and keeps only one
  read guard and one write per thread; RwLock preference/upgrade is not modelled.
  Any conclusion is limited to the two-shard cycle.
* Neither model claims source→CIR semantic equivalence. Reductions can remove or
  add counterexamples and can change fixability.

## 5. Frozen old data and boundaries

* `experiments/pilot-v1/` and `experiments/pilot-v2/results/` are unchanged: the
  lifecycle round's `frozen-data-sha256.txt` (3849 files, v1 = 1906) still
  matches every file byte-for-byte.
* No LLM/provider/prompt code was added to ConcIR (repository boundary:
  `REPOSITORY_BOUNDARIES.md`). All future LLM/prompt/feedback work belongs to
  `/Users/kevin/local-repos/ConcPlanVerify`.
* Interface-gap note for the next phase: ConcPlanVerify's Rust integration still
  calls `cir2cvn --analyze` with the old flat `resources`/`functions`/`op` schema
  and pins an old ConcIR revision; it cannot consume the current CLI/artifact
  contract without a separate offline adapter and integration test. No
  ConcPlanVerify files were changed or deleted this round.

## 6. What is not done

* Only one supported real case (`rmw-zenoh-998`); a second supported real case
  was not found within budget. The closest second candidate (`dashmap-369`) is
  outside the current CIR/RwLock support, and the remaining candidates are not
  lock-order inversions. Reported as a blocker, not papered over.
* Fixed revisions could not be verified for either included case: both issues
  were closed with no linked PR/commit exposed. `fixed_ref` is `null` and
  `fixed.cir.json` for `rmw-zenoh-998` is explicitly a hypothesis, not upstream.
* No 3-repeat determinism, no statistical significance, no speed conclusion.
* No large real program; the models are minimal two-thread reductions.

Next substantive work: source-traceable real cases with a verified fixed
revision and a supported mapping; the transition to LLM-driven repair belongs to
ConcPlanVerify once the offline adapter is built.
