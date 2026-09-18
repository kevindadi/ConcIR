# Pilot v2 generated tables

- batch_id: `b5`
- cohort_fingerprint: `50a0eb90f615b2031916b1d4616a50718ef9e67ece4ced4d8af4405369698bae`
- binary_sha256: `65a8f633f986e890d2e4256700db3ab62c1ee3b69418af30e61531b3c96b62c6`
- code_fingerprint: `3f96a157a0b7cf9373d29c2815e236ef4e87e543447f75e88966a52064380089`
- records: 3 (complete 3, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| p1_same | main | repaired v2 s92 t9 | repaired v2 s92 t12 | repaired v2 s92 t9 |

## Paired B/C (same identity, only strategy differs)

- pairs: 1; both repaired: 1; C<=B verification: 1/1; C<B: 0
- on both-repaired pairs: sum verification B/C = 2/2; sum states B/C = 92/92; sum wall B/C = 12/9 ms
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

