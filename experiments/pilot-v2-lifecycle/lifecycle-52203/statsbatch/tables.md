# Pilot v2 generated tables

- batch_id: `None`
- cohort_fingerprint: `None`
- binary_sha256: `None`
- code_fingerprint: `None`
- records: 4 (complete 4, other 0)

## Repair results (configs present in this batch)

`vN` verification calls, `sN` cumulative states, `tN` ms wall.

| case | config | A | B | C |
| --- | --- | --- | --- | --- |
| p1_same | main | repaired v2 s10 t2 | repaired v2 s10 t2 | repaired v2 s10 t2 |

## Paired B/C (same identity, only strategy differs)

- pairs: 1; both repaired: 1; C<=B verification: 1/1; C<B: 0
- on both-repaired pairs: sum verification B/C = 2/2; sum states B/C = 10/10; sum wall B/C = 2/2 ms
- pairs include repeats/configs of the same model, not independent programs

## Denominators

| suite | classification | count |
| --- | --- | --- |
| pilot | repaired | 4 |

| suite | evidence_status | count |
| --- | --- | --- |
| pilot | complete | 4 |

## Determinism (full identity, repeat excluded)

No nondeterministic groups over all non-time fields.

| case | config | strategy | repeats | min_ms | max_ms | mean_ms |
| --- | --- | --- | --- | --- | --- | --- |

