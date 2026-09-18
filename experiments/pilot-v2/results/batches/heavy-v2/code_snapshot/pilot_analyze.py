#!/usr/bin/env python3
"""Statistics and table generation for a pilot batch.

Reads only the batch's `valid_index.jsonl` (one chosen record per logical run),
so append-only attempt logs can never inflate denominators or mix configurations.
All wall-time numbers in the generated tables come from this index; nothing is
hand-written.
"""

from __future__ import annotations

import csv
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path

NON_TIME_FIELDS = (
    "stage", "raw_outcome", "final_classification", "stop_reason", "truncation",
    "saw_unknown", "proposals", "unique_programs", "verification_calls", "cache_hits",
    "states_explored", "transitions_explored", "nodes", "patch_len", "accepted_chain_len",
    "exit_code", "replay_exit", "replay_ok",
)


def sha_canonical(obj) -> str:
    return hashlib.sha256(
        json.dumps(obj, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def load_index(batch_dir: Path):
    path = batch_dir / "valid_index.jsonl"
    recs = []
    if path.exists():
        for line in path.read_text().splitlines():
            if line.strip():
                recs.append(json.loads(line))
    return recs


def load_json(path: Path, default=None):
    if path.exists():
        return json.loads(path.read_text())
    return default


def pair_group(r):
    return sha_canonical({
        "stage": r.get("stage"), "case": r.get("case"), "config_name": r.get("config_name"),
        "config": r.get("config"), "repeat": r.get("repeat"), "identity": r.get("identity"),
        "search_timeout": r.get("search_timeout"), "replay_timeout": r.get("replay_timeout"),
    })[:16]


def nf(r):
    return {k: r.get(k) for k in NON_TIME_FIELDS}


def summarize_batch(batch_dir: Path, out: Path) -> int:
    recs = load_index(batch_dir)
    meta = load_json(batch_dir / "batch.json", {})
    witness = load_json(out / "witness_results.json", [])
    env = load_json(batch_dir / "environment.json", {})

    complete = [r for r in recs if r.get("evidence_status") == "complete"]
    not_complete = [r for r in recs if r.get("evidence_status") != "complete"]

    # ---- flat CSV ----
    fields = [
        "stage", "suite", "case", "family", "config_name", "strategy", "repeat",
        "raw_outcome", "final_classification", "evidence_status", "stop_reason", "truncation",
        "saw_unknown", "root_outcome", "proposals", "unique_programs", "verification_calls",
        "cache_hits", "states_explored", "transitions_explored", "nodes", "patch_len",
        "accepted_chain_len", "exit_code", "wall_ms", "search_wall_ms", "replay_exit",
        "replay_wall_ms", "replay_ok", "artifact_sha256", "artifact_path", "reused_from",
    ]
    with (batch_dir / "summary.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for r in sorted(recs, key=lambda r: (r.get("suite", ""), r.get("case", ""),
                                             r.get("config_name", ""), r.get("strategy", ""),
                                             r.get("repeat", 0), r.get("stage", ""))):
            w.writerow({k: (json.dumps(r.get(k)) if k == "identity" else r.get(k)) for k in fields})

    # ---- determinism across repeats (main config) ----
    groups = defaultdict(list)
    for r in recs:
        if r.get("stage") == "repair":
            groups[(r["case"], r["config_name"], r["strategy"])].append(r)
    nondeterministic = []
    wall_stats = []
    for key, rs in sorted(groups.items()):
        if len(rs) < 2:
            continue
        base = nf(rs[0])
        mismatch = any(nf(r) != base for r in rs[1:])
        if mismatch:
            nondeterministic.append(list(key))
        ws = [r.get("wall_ms", 0) for r in rs]
        wall_stats.append({"case": key[0], "config": key[1], "strategy": key[2],
                           "min_ms": min(ws), "max_ms": max(ws),
                           "mean_ms": round(sum(ws) / len(ws), 1)})

    # ---- paired B/C by full identity (strategy is the only difference) ----
    pair_groups = defaultdict(dict)
    for r in recs:
        if r.get("stage") == "repair" and r.get("strategy") in ("b", "c"):
            pair_groups[pair_group(r)][r["strategy"]] = r
    paired = []
    for g, d in sorted(pair_groups.items()):
        b, c = d.get("b"), d.get("c")
        if not b or not c:
            continue
        b_ok = b.get("final_classification") == "repaired"
        c_ok = c.get("final_classification") == "repaired"
        paired.append({
            "pair_group": g, "case": b["case"], "config_name": b["config_name"],
            "repeat": b["repeat"],
            "b_status": b.get("evidence_status"), "c_status": c.get("evidence_status"),
            "b": b.get("final_classification"), "c": c.get("final_classification"),
            "b_ver": b.get("verification_calls"), "c_ver": c.get("verification_calls"),
            "b_states": b.get("states_explored"), "c_states": c.get("states_explored"),
            "b_wall_ms": b.get("wall_ms"), "c_wall_ms": c.get("wall_ms"),
            "both_repaired": b_ok and c_ok, "success_differs": b_ok != c_ok,
        })

    both = [p for p in paired if p["both_repaired"]]
    paired_summary = {
        "pairs": len(paired),
        "both_repaired": len(both),
        "c_le_b_ver": sum(1 for p in both if (p["c_ver"] or 0) <= (p["b_ver"] or 0)),
        "c_lt_b_ver": sum(1 for p in both if (p["c_ver"] or 0) < (p["b_ver"] or 0)),
        "sum_b_ver": sum(p["b_ver"] or 0 for p in both),
        "sum_c_ver": sum(p["c_ver"] or 0 for p in both),
        "sum_b_states": sum(p["b_states"] or 0 for p in both),
        "sum_c_states": sum(p["c_states"] or 0 for p in both),
        "sum_b_wall_ms": sum(p["b_wall_ms"] or 0 for p in both),
        "sum_c_wall_ms": sum(p["c_wall_ms"] or 0 for p in both),
        "success_differs": [p for p in paired if p["success_differs"]],
    }

    # ---- denominators ----
    by_suite = defaultdict(Counter)
    status_by_suite = defaultdict(Counter)
    for r in recs:
        by_suite[r.get("suite", "?")][r.get("final_classification", "?")] += 1
        status_by_suite[r.get("suite", "?")][r.get("evidence_status", "?")] += 1

    # ---- matrix (explore) table ----
    matrix = []
    for r in recs:
        if r.get("stage") == "explore":
            matrix.append({
                "case": r["case"], "bounds_variant": r["config_name"],
                "bounds": (r.get("identity", {}) or {}).get("contract_bounds", {}),
                "outcome": r.get("raw_outcome"), "status": r.get("evidence_status"),
                "states": r.get("states_explored"), "transitions": r.get("transitions_explored"),
                "wall_ms": r.get("wall_ms"),
            })

    summary = {
        "batch_id": meta.get("batch_id"),
        "batch_fingerprint": meta.get("batch_fingerprint"),
        "records": len(recs),
        "complete": len(complete),
        "not_complete": len(not_complete),
        "nondeterministic": nondeterministic,
        "wall_stats": wall_stats,
        "paired": paired_summary,
        "by_suite": {k: dict(v) for k, v in by_suite.items()},
        "status_by_suite": {k: dict(v) for k, v in status_by_suite.items()},
        "matrix": matrix,
        "witness": witness,
        "meta": meta,
        "environment": env,
    }
    (batch_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    tables = build_tables(recs, summary, witness)
    (batch_dir / "tables.md").write_text(tables)
    print(json.dumps({
        "records": len(recs), "complete": len(complete), "not_complete": len(not_complete),
        "nondeterministic": nondeterministic, "paired": paired_summary["pairs"],
        "both_repaired": paired_summary["both_repaired"],
    }, indent=2))
    return 0


def _fmt(v):
    return "" if v is None else str(v)


def build_tables(recs, summary, witness) -> str:
    L = []
    meta = summary.get("meta", {})
    L.append("# Pilot v2 generated tables\n")
    L.append(f"- batch_id: `{meta.get('batch_id')}`")
    L.append(f"- batch_fingerprint: `{meta.get('batch_fingerprint')}`")
    L.append(f"- binary_sha256: `{meta.get('binary_sha256')}`")
    L.append(f"- code_fingerprint: `{meta.get('code_fingerprint')}`")
    L.append(f"- records: {summary['records']} (complete {summary['complete']}, "
             f"other {summary['not_complete']})")
    L.append("")

    # per-case repair table (all configs present in the batch), A/B/C
    L.append("## Repair results (configs present in this batch)\n")
    L.append("`vN` verification calls, `sN` cumulative states, `tN` ms wall.\n")
    L.append("| case | config | A | B | C |")
    L.append("| --- | --- | --- | --- | --- |")
    cases = sorted({r["case"] for r in recs if r.get("stage") == "repair"})
    configs = sorted({r["config_name"] for r in recs if r.get("stage") == "repair"})
    for case in cases:
        for config in configs:
            cells = []
            for st in ("a", "b", "c"):
                matches = [r for r in recs if r.get("stage") == "repair" and r["case"] == case
                           and r["config_name"] == config and r["strategy"] == st
                           and (r["repeat"] == 1)]
                if not matches:
                    cells.append("")
                    continue
                r = matches[0]
                if r.get("evidence_status") != "complete":
                    cells.append(f"{r.get('evidence_status')}")
                else:
                    cells.append(f"{r.get('final_classification')} v{_fmt(r.get('verification_calls'))} "
                                 f"s{_fmt(r.get('states_explored'))} t{_fmt(r.get('wall_ms'))}")
            L.append(f"| {case} | {config} | {cells[0]} | {cells[1]} | {cells[2]} |")
    L.append("")

    # paired B/C
    ps = summary["paired"]
    L.append("## Paired B/C (same identity, only strategy differs)\n")
    L.append(f"- pairs: {ps['pairs']}; both repaired: {ps['both_repaired']}; "
             f"C<=B verification: {ps['c_le_b_ver']}/{ps['both_repaired']}; "
             f"C<B: {ps['c_lt_b_ver']}")
    L.append(f"- on both-repaired pairs: sum verification B/C = {ps['sum_b_ver']}/{ps['sum_c_ver']}; "
             f"sum states B/C = {ps['sum_b_states']}/{ps['sum_c_states']}; "
             f"sum wall B/C = {ps['sum_b_wall_ms']}/{ps['sum_c_wall_ms']} ms")
    if ps["success_differs"]:
        L.append("\nSuccess sets differ on:\n")
        L.append("| case | config | repeat | B | C |")
        L.append("| --- | --- | --- | --- | --- |")
        for p in ps["success_differs"]:
            L.append(f"| {p['case']} | {p['config_name']} | {p['repeat']} | {p['b']} | {p['c']} |")
    L.append("")

    # denominators
    L.append("## Denominators\n")
    L.append("| suite | classification | count |")
    L.append("| --- | --- | --- |")
    for suite, counts in sorted(summary["by_suite"].items()):
        for k, v in sorted(counts.items()):
            L.append(f"| {suite} | {k} | {v} |")
    L.append("")
    L.append("| suite | evidence_status | count |")
    L.append("| --- | --- | --- |")
    for suite, counts in sorted(summary["status_by_suite"].items()):
        for k, v in sorted(counts.items()):
            L.append(f"| {suite} | {k} | {v} |")
    L.append("")

    # determinism
    L.append("## Determinism (3 main repeats)\n")
    if summary["nondeterministic"]:
        L.append("Nondeterministic groups: " + ", ".join(str(g) for g in summary["nondeterministic"]))
    else:
        L.append("No nondeterministic groups over all non-time fields.")
    L.append("")
    L.append("| case | config | strategy | min_ms | max_ms | mean_ms |")
    L.append("| --- | --- | --- | --- | --- | --- |")
    for w in summary["wall_stats"]:
        L.append(f"| {w['case']} | {w['config']} | {w['strategy']} | {w['min_ms']} | "
                 f"{w['max_ms']} | {w['mean_ms']} |")
    L.append("")

    # matrix
    if summary["matrix"]:
        L.append("## Root verification budget matrix (explore only, no search)\n")
        L.append("| case | variant | bounds.max_states | outcome | states | transitions | wall_ms |")
        L.append("| --- | --- | --- | --- | --- | --- | --- |")
        for m in summary["matrix"]:
            b = m.get("bounds") or {}
            L.append(f"| {m['case']} | {m['bounds_variant']} | {b.get('max_states','')} | "
                     f"{m['outcome']} | {_fmt(m['states'])} | {_fmt(m['transitions'])} | {m['wall_ms']} |")
        L.append("")

    # witness
    if witness:
        L.append("## Witness validation (legal = allowed adjacent swap chain on the original)\n")
        L.append("| case | kind | chain | permissions | final outcome | states |")
        L.append("| --- | --- | --- | --- | --- | --- |")
        for w in witness:
            L.append(f"| {w.get('case')} | {w.get('witness_kind')} | {_fmt(w.get('chain_len'))} | "
                     f"{_fmt(w.get('permissions_ok'))} | {_fmt(w.get('final_outcome'))} | "
                     f"{_fmt(w.get('final_states'))} |")
        L.append("")
    return "\n".join(L) + "\n"


if __name__ == "__main__":
    import sys
    bd = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else bd
    raise SystemExit(summarize_batch(bd, out))
