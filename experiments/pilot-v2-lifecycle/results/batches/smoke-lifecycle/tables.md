# Pilot v2 generated tables

- batch_id: `smoke-lifecycle`
- cohort_fingerprint: `d3f090b3bd06ea75cac2a14922cd59ce8e2dfabb8f7c0d96b28da6cce0f5cc4b`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `9a6eb384a5e744c0ea86d66101d7c841c99350d7d80c2b4d5dd7ee4de66c9fd6`
- records: 24 (complete 24, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| already_correct | smoke | already_satisfied v1 s39 t8 | already_satisfied v1 s39 t7 | already_satisfied v1 s39 t7 |
| budget_truncated | smoke | budget_exhausted v2 s4140 t104 | budget_exhausted v2 s4140 t102 | budget_exhausted v2 s4140 t106 |
| cross_module_two_cycles | smoke | no_acceptable_candidate v5 s9927 t227 | repaired v7 s13445 t307 | repaired v6 s11610 t274 |
| forbidden_scope | smoke | no_acceptable_candidate v1 s49 t8 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| no_lock_candidate | smoke | no_acceptable_candidate v1 s5 t7 | no_acceptable_candidate v1 s5 t7 | no_acceptable_candidate v1 s5 t7 |
| preserved_unfixable | smoke | no_acceptable_candidate v3 s135 t8 | no_acceptable_candidate v4 s176 t9 | no_acceptable_candidate v4 s176 t9 |
| single_cycle | smoke | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t8 |
| two_cycles | smoke | no_acceptable_candidate v5 s9927 t226 | repaired v7 s13445 t320 | repaired v6 s11610 t264 |

## Paired B/C (same identity, only strategy differs)

- pairs: 8; both repaired: 3; C<=B verification: 3/3; C<B: 2
- on both-repaired pairs: sum verification B/C = 16/14; sum states B/C = 26982/23312; sum wall B/C = 635/546 ms
- pairs include repeats/configs of the same model, not independent programs

## Denominators

| suite | classification | count |
| --- | --- | --- |
| smoke | already_satisfied | 3 |
| smoke | budget_exhausted | 3 |
| smoke | no_acceptable_candidate | 11 |
| smoke | repaired | 7 |

| suite | evidence_status | count |
| --- | --- | --- |
| smoke | complete | 24 |

## Determinism (full identity, repeat excluded)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

