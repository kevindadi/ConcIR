# Pilot v2 generated tables

- batch_id: `smoke-v2`
- batch_fingerprint: `5e0e0fe0bd5fe6ad8d1f893191f15a97dedf6babeda491cac48cb11c5dc5e956`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `99ad9172a5e03b1b48b41ea804c2a1e426e0c1341474ba2a2484ca67a61bdc83`
- records: 24 (complete 24, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| already_correct | smoke | already_satisfied v1 s39 t7 | already_satisfied v1 s39 t8 | already_satisfied v1 s39 t7 |
| budget_truncated | smoke | budget_exhausted v2 s4140 t104 | budget_exhausted v2 s4140 t106 | budget_exhausted v2 s4140 t104 |
| cross_module_two_cycles | smoke | no_acceptable_candidate v5 s9927 t234 | repaired v7 s13445 t310 | repaired v6 s11610 t271 |
| forbidden_scope | smoke | no_acceptable_candidate v1 s49 t8 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| no_lock_candidate | smoke | no_acceptable_candidate v1 s5 t7 | no_acceptable_candidate v1 s5 t7 | no_acceptable_candidate v1 s5 t7 |
| preserved_unfixable | smoke | no_acceptable_candidate v3 s135 t8 | no_acceptable_candidate v4 s176 t9 | no_acceptable_candidate v4 s176 t9 |
| single_cycle | smoke | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t8 |
| two_cycles | smoke | no_acceptable_candidate v5 s9927 t256 | repaired v7 s13445 t310 | repaired v6 s11610 t269 |

## Paired B/C (same identity, only strategy differs)

- pairs: 8; both repaired: 3; C<=B verification: 3/3; C<B: 2
- on both-repaired pairs: sum verification B/C = 16/14; sum states B/C = 26982/23312; sum wall B/C = 628/548 ms

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

## Determinism (3 main repeats)

No nondeterministic groups over all non-time fields.

| case | config | strategy | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- |

## Witness validation (legal = allowed adjacent swap chain on the original)

| case | kind | chain | permissions | final outcome | states |
| --- | --- | --- | --- | --- | --- |
| p1_same | legal | 1 | True | PASS | 43 |
| p1_cross_shared | legal | 1 | True | PASS | 43 |
| p1_interf | legal | 1 | True | PASS | 248 |
| p2_same | legal | 2 | True | PASS | 1683 |
| p2_cross_independent | legal | 2 | True | PASS | 1683 |
| p2_cross_shared | legal | 2 | True | PASS | 1683 |
| p2_interf | legal | 2 | True | PASS | 10088 |
| c_already_correct | none |  |  |  |  |
| c_scope_restricted | auxiliary |  |  |  |  |
| c_lock_reorder_forbidden | auxiliary |  |  |  |  |
| c_preserved_unsatisfiable | none |  |  |  |  |
| c_bounds_unknown | legal | 2 | True | UNKNOWN | 50 |
| e_name_order | legal | 2 | True | PASS | 1683 |
| e_module_order | legal | 2 | True | PASS | 1683 |
| p3_same | legal | 3 | True | PASS | 68923 |
| p3_cross_independent | legal | 3 | True | PASS | 68923 |

