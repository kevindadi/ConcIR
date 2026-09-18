# Pilot v2 generated tables

- batch_id: `heavy-v2`
- batch_fingerprint: `577fb2aa5b69bb853445bfd6987afc583dbc06e75e8ec6608268138310536e0a`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `99ad9172a5e03b1b48b41ea804c2a1e426e0c1341474ba2a2484ca67a61bdc83`
- records: 3 (complete 3, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| p3_same | heavy | no_acceptable_candidate v7 s647251 t28102 | repaired v28 s2298516 t99630 | repaired v20 s1664282 t72053 |

## Paired B/C (same identity, only strategy differs)

- pairs: 1; both repaired: 1; C<=B verification: 1/1; C<B: 1
- on both-repaired pairs: sum verification B/C = 28/20; sum states B/C = 2298516/1664282; sum wall B/C = 99630/72053 ms

## Denominators

| suite | classification | count |
| --- | --- | --- |
| heavy | no_acceptable_candidate | 1 |
| heavy | repaired | 2 |

| suite | evidence_status | count |
| --- | --- | --- |
| heavy | complete | 3 |

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

