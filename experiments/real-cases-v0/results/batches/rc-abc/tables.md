# Pilot generated tables

- batch_id: `rc-abc`
- cohort_fingerprint: `42fd4d102918fcb8f6d5a4d961e63a3b4cc0b5169435fd41e203bcfb5536cc2d`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `fc1c304bf4b0933e3e0c203c09fb0f24e47016451607193355eb1ab060ad0296`
- records: 6 (complete 6, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` = search_wall_ms (search cost only; replay and end-to-end are separate columns).

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| dashmap-369 | main | unsupported v1 s0 t7 | unsupported v1 s0 t7 | unsupported v1 s0 t7 |
| rmw-zenoh-998 | main | repaired v2 s92 t8 | repaired v2 s92 t8 | repaired v2 s92 t9 |

## Strategy pairing (all outcomes kept)

- planned B/C pairs: 2; pairs in index: 2; both attempted: 2
- both evidence-complete: 2; both repaired: 1
- pairs with a not_executed side: 0; pairs with an invalid/limited side: 0; outcome differences: 0

### Cost comparison subset: both repaired, `search_wall_ms` only

- subset pairs: 1; C<=B verification: 1/1; C<B: 0
- sum verification B/C = 2/2; sum states B/C = 92/92; sum search wall B/C = 8/9 ms
- pairs include repeats/configs of the same model, not independent programs; timeout/UNKNOWN/failure outcomes are kept

## Denominators

| suite | classification | count |
| --- | --- | --- |
| pilot | repaired | 3 |
| pilot | unsupported | 3 |

| suite | evidence_status | count |
| --- | --- | --- |
| pilot | complete | 6 |

## Determinism (full identity, repeat excluded; `search_wall_ms`)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

