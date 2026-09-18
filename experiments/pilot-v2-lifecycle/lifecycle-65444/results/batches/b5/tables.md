# Pilot v2 generated tables

- batch_id: `b5`
- cohort_fingerprint: `e690a6c2edfe64c7c40505ad1ffbfa4949b5b5a67ff81fe07c77b7968ea1b579`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `c2905bfdf373881456bc610e8d79b44f2839578d9bca2716aa9b2e0184f96cd3`
- records: 3 (complete 3, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| p1_same | main | repaired v2 s92 t9 | repaired v2 s92 t14 | repaired v2 s92 t9 |

## Paired B/C (same identity, only strategy differs)

- pairs: 1; both repaired: 1; C<=B verification: 1/1; C<B: 0
- on both-repaired pairs: sum verification B/C = 2/2; sum states B/C = 92/92; sum wall B/C = 14/9 ms
- pairs include repeats/configs of the same model, not independent programs

## Denominators

| suite | classification | count |
| --- | --- | --- |
| pilot | repaired | 3 |

| suite | evidence_status | count |
| --- | --- | --- |
| pilot | complete | 3 |

## Determinism (full identity, repeat excluded)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

