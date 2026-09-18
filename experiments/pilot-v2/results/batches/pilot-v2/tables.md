# Pilot v2 generated tables

- batch_id: `pilot-v2`
- batch_fingerprint: `f960603cab8f0cce370f6571b2dab1666d424257587fc33c6fb9b898f447134d`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `99ad9172a5e03b1b48b41ea804c2a1e426e0c1341474ba2a2484ca67a61bdc83`
- records: 180 (complete 176, other 4)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| c_already_correct | main | already_satisfied v1 s8 t7 | already_satisfied v1 s8 t6 | already_satisfied v1 s8 t7 |
| c_already_correct | tight | already_satisfied v1 s8 t6 | already_satisfied v1 s8 t6 | already_satisfied v1 s8 t6 |
| c_bounds_unknown | main | analysis_unknown v1 s50 t8 | analysis_unknown v1 s50 t8 | analysis_unknown v1 s50 t8 |
| c_bounds_unknown | tight | analysis_unknown v1 s50 t8 | analysis_unknown v1 s50 t8 | analysis_unknown v1 s50 t8 |
| c_lock_reorder_forbidden | main | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| c_lock_reorder_forbidden | tight | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| c_preserved_unsatisfiable | main | no_acceptable_candidate v3 s135 t8 | no_acceptable_candidate v4 s176 t9 | no_acceptable_candidate v4 s176 t9 |
| c_preserved_unsatisfiable | tight | no_acceptable_candidate v3 s135 t8 | budget_exhausted v3 s135 t8 | budget_exhausted v3 s135 t9 |
| c_scope_restricted | main | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| c_scope_restricted | tight | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 | no_acceptable_candidate v1 s49 t7 |
| e_module_order | main | no_acceptable_candidate v5 s9927 t237 | repaired v7 s13445 t315 | repaired v6 s11610 t273 |
| e_module_order | tight | budget_exhausted v4 s7998 t193 | budget_exhausted v4 s7998 t192 | budget_exhausted v4 s7998 t192 |
| e_name_order | main | no_acceptable_candidate v5 s9927 t235 | repaired v7 s13445 t317 | repaired v6 s11610 t272 |
| e_name_order | tight | budget_exhausted v4 s7998 t193 | budget_exhausted v4 s7998 t192 | budget_exhausted v4 s7998 t192 |
| p1_cross_shared | main | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t8 |
| p1_cross_shared | tight | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t7 |
| p1_interf | main | repaired v2 s532 t16 | repaired v2 s532 t16 | repaired v2 s532 t16 |
| p1_interf | tight | repaired v2 s532 t17 | repaired v2 s532 t16 | repaired v2 s532 t17 |
| p1_same | main | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t8 |
| p1_same | tight | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t7 |
| p2_cross_independent | main | no_acceptable_candidate v5 s9927 t235 | repaired v7 s13445 t311 | repaired v6 s11610 t270 |
| p2_cross_independent | tight | budget_exhausted v4 s7998 t192 | budget_exhausted v4 s7998 t191 | budget_exhausted v4 s7998 t195 |
| p2_cross_shared | main | no_acceptable_candidate v5 s9927 t239 | repaired v6 s11610 t277 | repaired v6 s11610 t273 |
| p2_cross_shared | tight | budget_exhausted v4 s7998 t193 | budget_exhausted v4 s7998 t194 | budget_exhausted v4 s7998 t194 |
| p2_interf | main | no_acceptable_candidate v6 s72768 t2492 | repaired v8 s93856 t3178 | repaired v6 s69600 t2351 |
| p2_interf | tight | budget_exhausted v4 s47948 t1628 | budget_exhausted v4 s47948 t1631 | budget_exhausted v4 s47948 t1632 |
| p2_same | main | no_acceptable_candidate v5 s9927 t237 | repaired v7 s13445 t313 | repaired v6 s11610 t271 |
| p2_same | tight | budget_exhausted v4 s7998 t193 | budget_exhausted v4 s7998 t192 | budget_exhausted v4 s7998 t192 |
| p3_cross_independent | main | no_acceptable_candidate v7 s647251 t28112 | search_timeout | search_timeout |
| p3_cross_independent | tight | budget_exhausted v4 s375538 t16398 | budget_exhausted v4 s375538 t16391 | budget_exhausted v4 s375538 t16419 |
| p3_same | main | no_acceptable_candidate v7 s647251 t28017 | search_timeout | search_timeout |
| p3_same | tight | budget_exhausted v4 s375538 t16418 | budget_exhausted v4 s375538 t16373 | budget_exhausted v4 s375538 t16395 |

## Paired B/C (same identity, only strategy differs)

- pairs: 60; both repaired: 30; C<=B verification: 30/30; C<B: 15
- on both-repaired pairs: sum verification B/C = 150/132; sum states B/C = 480602/385814; sum wall B/C = 14213/11318 ms

## Denominators

| suite | classification | count |
| --- | --- | --- |
| pilot | already_satisfied | 12 |
| pilot | analysis_unknown | 12 |
| pilot | budget_exhausted | 26 |
| pilot | no_acceptable_candidate | 54 |
| pilot | repaired | 72 |
| pilot | search_timeout | 4 |

| suite | evidence_status | count |
| --- | --- | --- |
| pilot | complete | 176 |
| pilot | search_timeout | 4 |

## Determinism (3 main repeats)

No nondeterministic groups over all non-time fields.

| case | config | strategy | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- |
| c_already_correct | main | a | 7 | 7 | 7.0 |
| c_already_correct | main | b | 6 | 7 | 6.3 |
| c_already_correct | main | c | 7 | 7 | 7.0 |
| c_bounds_unknown | main | a | 8 | 8 | 8.0 |
| c_bounds_unknown | main | b | 7 | 8 | 7.3 |
| c_bounds_unknown | main | c | 7 | 8 | 7.7 |
| c_lock_reorder_forbidden | main | a | 7 | 8 | 7.3 |
| c_lock_reorder_forbidden | main | b | 7 | 7 | 7.0 |
| c_lock_reorder_forbidden | main | c | 7 | 7 | 7.0 |
| c_preserved_unsatisfiable | main | a | 8 | 9 | 8.3 |
| c_preserved_unsatisfiable | main | b | 9 | 10 | 9.3 |
| c_preserved_unsatisfiable | main | c | 9 | 9 | 9.0 |
| c_scope_restricted | main | a | 7 | 7 | 7.0 |
| c_scope_restricted | main | b | 7 | 7 | 7.0 |
| c_scope_restricted | main | c | 7 | 7 | 7.0 |
| e_module_order | main | a | 237 | 238 | 237.3 |
| e_module_order | main | b | 313 | 315 | 314.3 |
| e_module_order | main | c | 273 | 292 | 279.3 |
| e_name_order | main | a | 235 | 237 | 235.7 |
| e_name_order | main | b | 309 | 317 | 313.3 |
| e_name_order | main | c | 272 | 277 | 274.0 |
| p1_cross_shared | main | a | 8 | 8 | 8.0 |
| p1_cross_shared | main | b | 8 | 8 | 8.0 |
| p1_cross_shared | main | c | 8 | 8 | 8.0 |
| p1_interf | main | a | 16 | 17 | 16.3 |
| p1_interf | main | b | 16 | 17 | 16.7 |
| p1_interf | main | c | 16 | 19 | 17.0 |
| p1_same | main | a | 8 | 8 | 8.0 |
| p1_same | main | b | 8 | 8 | 8.0 |
| p1_same | main | c | 8 | 8 | 8.0 |
| p2_cross_independent | main | a | 235 | 235 | 235.0 |
| p2_cross_independent | main | b | 311 | 312 | 311.3 |
| p2_cross_independent | main | c | 270 | 273 | 272.0 |
| p2_cross_shared | main | a | 237 | 239 | 237.7 |
| p2_cross_shared | main | b | 276 | 296 | 283.0 |
| p2_cross_shared | main | c | 272 | 276 | 273.7 |
| p2_interf | main | a | 2459 | 2492 | 2473.0 |
| p2_interf | main | b | 3150 | 3178 | 3159.3 |
| p2_interf | main | c | 2351 | 2367 | 2358.3 |
| p2_same | main | a | 234 | 237 | 235.7 |
| p2_same | main | b | 313 | 313 | 313.0 |
| p2_same | main | c | 271 | 274 | 272.0 |

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

