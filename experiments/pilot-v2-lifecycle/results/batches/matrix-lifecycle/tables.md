# Pilot v2 generated tables

- batch_id: `matrix-lifecycle`
- cohort_fingerprint: `008e2d171fc34e23a20b0bc52b29f9264e5fb7e45391b2af7fce1ab8b30f8350`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `9a6eb384a5e744c0ea86d66101d7c841c99350d7d80c2b4d5dd7ee4de66c9fd6`
- records: 12 (complete 12, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |

## Paired B/C (same identity, only strategy differs)

- pairs: 0; both repaired: 0; C<=B verification: 0/0; C<B: 0
- on both-repaired pairs: sum verification B/C = 0/0; sum states B/C = 0/0; sum wall B/C = 0/0 ms
- pairs include repeats/configs of the same model, not independent programs

## Denominators

| suite | classification | count |
| --- | --- | --- |
| matrix | FAIL | 11 |
| matrix | UNKNOWN | 1 |

| suite | evidence_status | count |
| --- | --- | --- |
| matrix | complete | 12 |

## Determinism (full identity, repeat excluded)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

## Root verification budget matrix (explore only, no search)

`evidence_status=complete` means the record is well-formed; `report_complete` is the backend's own bounded-exploration flag.

| case | variant | bounds.max_states | outcome | evidence_status | report_complete | states | transitions | wall_ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| p1_same | ms100000 | 100000 | FAIL | complete | True | 49 | 72 | 2 |
| p2_same | ms200000 | 200000 | FAIL | complete | True | 2211 | 6582 | 54 |
| p3_same | ms20000 | 20000 | UNKNOWN | complete | False | 20001 | 67922 | 695 |
| p1_interf | ms20000 | 20000 | FAIL | complete | True | 284 | 657 | 7 |
| p1_interf | ms200000 | 200000 | FAIL | complete | True | 284 | 657 | 7 |
| p1_same | ms200000 | 200000 | FAIL | complete | True | 49 | 72 | 3 |
| p2_same | ms20000 | 20000 | FAIL | complete | True | 2211 | 6582 | 57 |
| p1_interf | ms100000 | 100000 | FAIL | complete | True | 284 | 657 | 7 |
| p3_same | ms200000 | 200000 | FAIL | complete | True | 103825 | 463892 | 4502 |
| p3_same | ms100000 | 100000 | FAIL | complete | False | 100000 | 439834 | 4381 |
| p2_same | ms100000 | 100000 | FAIL | complete | True | 2211 | 6582 | 54 |
| p1_same | ms20000 | 20000 | FAIL | complete | True | 49 | 72 | 2 |

