# real-cases-v0 PROTOCOL (frozen before the run)

Goal: a feasibility and applicability check — can a real upstream concurrency
defect be reduced to the current CIR subset, verified, and (where the defect is
inside the current patch space) repaired with a legal witness? This is **not** a
held-out evaluation, not a statistical performance study, and does not claim to
verify any upstream project.

## Inclusion / exclusion

Included only if all hold:
1. a verifiable upstream repository + issue (URL) with either a reproducer or
   concrete evidence (stack/gdb, line references), and the language/concurrency
   primitive is a fixed-order lock acquisition;
2. the locks map to CIR `sync` resources and the two-lock ordering maps to CIR
   statements without inventing a different program;
3. a frozen contract can be written whose properties are justified by the source
   (deadlock freedom; both tasks complete), independent of any tool output.

Excluded otherwise, with the reason recorded in `candidates.json`. Same upstream
defect's variants/reductions count as one source group. Ground truth comes from
the upstream source/issue, never from CIR/backend output.

## Frozen inputs (hashes at run time)

| case | model sha256 (16) | contract sha256 (16) | analysis bounds |
| --- | --- | --- | --- |
| rmw-zenoh-998 | `77f6e181d701d23e` | `71e5dfb20bab9449` | max_states 20000, threads 8, frames 8, depth 64, boundary 256 |
| dashmap-369 | `84f94127f4cd8e3d` | `607bcf87dd35cfa0` | same |

`max_states=20000` is used for every case; no case is given a smaller or larger
bound, and no frozen contract is rewritten to make a case pass. If a bound needs
to be widened, it must be a separate explicitly labelled batch.

## Configs

- A/B/C strategies share the same public config `main`
  (candidate_budget 64, verification_budget 64, max_depth 4, max_total_edits 4).
- Root verification uses `petri` exploration only.
- Per-process search timeout 30 s, replay timeout 30 s (root batch 60 s search).
- Total budget 300 s per batch.
- **1 repeat per case/strategy** (feasibility only; no determinism or speed
  claim). Cases whose defect is outside the current patch space are expected to
  be `unsupported`/`no_acceptable_candidate`, not "fixed".

## Evidence

- `results/batches/rc-root/` — root verification (`explore`) for buggy/fixed.
- `results/batches/rc-abc/` — A/B/C repair with artifact replay.
- Every `complete` repair is replayed by a separate CLI call; `scripts/pilot_audit.py`
  re-hashes artifacts and checks counts/exit/replay. Old pilot data is untouched.

## Separation of claims

The handoff reports separately: (a) defect representable in CIR syntax,
(b) verification completes and its outcome, (c) repair within the current
adjacent-mutex-swap space. A and B/C `complete` is evidence the run is
well-formed, not that the model is a complete semantic equivalent of the source.
