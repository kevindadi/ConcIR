# Pilot v2 generated tables

- batch_id: `matrix-v2`
- batch_fingerprint: `19c398db787b058e9a251f500a012dcd91cfe6687dbbf53db9bfacdcf84eebd0`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `99ad9172a5e03b1b48b41ea804c2a1e426e0c1341474ba2a2484ca67a61bdc83`
- records: 12 (complete 12, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |

## Paired B/C (same identity, only strategy differs)

- pairs: 0; both repaired: 0; C<=B verification: 0/0; C<B: 0
- on both-repaired pairs: sum verification B/C = 0/0; sum states B/C = 0/0; sum wall B/C = 0/0 ms

## Denominators

| suite | classification | count |
| --- | --- | --- |
| matrix | FAIL | 11 |
| matrix | UNKNOWN | 1 |

| suite | evidence_status | count |
| --- | --- | --- |
| matrix | complete | 12 |

## Determinism (3 main repeats)

No nondeterministic groups over all non-time fields.

| case | config | strategy | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- |

## Root verification budget matrix (explore only, no search)

| case | variant | bounds.max_states | outcome | states | transitions | wall_ms |
| --- | --- | --- | --- | --- | --- | --- |
| p1_same | ms100000 | 100000 | FAIL | 49 | 72 | 2 |
| p2_same | ms200000 | 200000 | FAIL | 2211 | 6582 | 56 |
| p3_same | ms20000 | 20000 | UNKNOWN | 20001 | 67922 | 697 |
| p1_interf | ms20000 | 20000 | FAIL | 284 | 657 | 8 |
| p1_interf | ms200000 | 200000 | FAIL | 284 | 657 | 8 |
| p1_same | ms200000 | 200000 | FAIL | 49 | 72 | 2 |
| p2_same | ms20000 | 20000 | FAIL | 2211 | 6582 | 55 |
| p1_interf | ms100000 | 100000 | FAIL | 284 | 657 | 8 |
| p3_same | ms200000 | 200000 | FAIL | 103825 | 463892 | 4647 |
| p3_same | ms100000 | 100000 | FAIL | 100000 | 439834 | 4361 |
| p2_same | ms100000 | 100000 | FAIL | 2211 | 6582 | 56 |
| p1_same | ms20000 | 20000 | FAIL | 49 | 72 | 2 |

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

