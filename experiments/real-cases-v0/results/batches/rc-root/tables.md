# Pilot generated tables

- batch_id: `rc-root`
- cohort_fingerprint: `a9ce5f0f8a678cc82a6b95514cbf271e952f132def96bc6c28395834365330a4`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `fc1c304bf4b0933e3e0c203c09fb0f24e47016451607193355eb1ab060ad0296`
- records: 3 (complete 3, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` = search_wall_ms (search cost only; replay and end-to-end are separate columns).

| case | config | A | B | C |
| --- | --- | --- | --- | --- |

## Strategy pairing (all outcomes kept)

- planned B/C pairs: 0; pairs in index: 0; both attempted: 0
- both evidence-complete: 0; both repaired: 0
- pairs with a not_executed side: 0; pairs with an invalid/limited side: 0; outcome differences: 0

### Cost comparison subset: both repaired, `search_wall_ms` only

- subset pairs: 0; C<=B verification: 0/0; C<B: 0
- sum verification B/C = 0/0; sum states B/C = 0/0; sum search wall B/C = 0/0 ms
- pairs include repeats/configs of the same model, not independent programs; timeout/UNKNOWN/failure outcomes are kept

## Denominators

| suite | classification | count |
| --- | --- | --- |
| matrix | FAIL | 1 |
| matrix | PASS | 1 |
| matrix | UNSUPPORTED | 1 |

| suite | evidence_status | count |
| --- | --- | --- |
| matrix | complete | 3 |

## Determinism (full identity, repeat excluded; `search_wall_ms`)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

## Root verification budget matrix (explore only, no search)

`evidence_status=complete` means the record is well-formed; `report_complete` is the backend's own bounded-exploration flag.

| case | variant | bounds.max_states | outcome | evidence_status | report_complete | states | transitions | search_wall_ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dashmap-369-buggy | buggy | 20000 | UNSUPPORTED | complete | False | 0 | 0 | 2 |
| rmw-zenoh-998-fixed | fixed | 20000 | PASS | complete | True | 39 | 56 | 3 |
| rmw-zenoh-998-buggy | buggy | 20000 | FAIL | complete | True | 49 | 72 | 4 |

