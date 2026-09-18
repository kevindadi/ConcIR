#!/usr/bin/env python3
"""Statistics and table generation for a pilot batch.

Reads only the batch's `valid_index.jsonl` (one chosen record per logical run),
so append-only attempt logs can never inflate denominators or mix configurations.
All wall-time numbers in the generated tables come from this index; nothing is
hand-written.

Validity rules:

* A `complete`/`replay_pending` record must still reference valid evidence
  (artifact exists, hash matches, outcome/exit binding holds). A record that
  fails this is reported as an error and is **not** counted as a success —
  statistics never treat a missing value as zero and never re-run the backend.
* Strategy outcome pairing is built from the full plan and the valid index; it
  keeps timeout/UNKNOWN/failure outcomes instead of filtering to `complete`.
  The verification-cost comparison is a separate `both_repaired` subset.
* Performance uses `search_wall_ms` on every path (normal and recovered).
  `replay_wall_ms` and `end_to_end_wall_ms` are reported separately; replay/
  retry time is never mixed into the search cost.
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

REPAIR_EXIT = {
    "repaired": 0, "already_satisfied": 0, "no_acceptable_candidate": 1,
    "budget_exhausted": 1, "analysis_unknown": 3, "invalid": 4,
    "invalid_config": 4, "unsupported": 5,
}
EXPLORE_EXIT = {"PASS": 0, "FAIL": 1, "UNKNOWN": 3, "INVALID": 4, "UNSUPPORTED": 5}

# Evidence statuses that are legitimate observations but not "complete".
FAULT_STATUSES = {
    "search_timeout", "replay_timeout", "replay_failed", "spawn_error", "resource_error",
    "json_error", "no_artifact", "artifact_missing", "identity_mismatch",
    "exit_outcome_mismatch", "evidence_incomplete", "evidence_invalid",
}


def required_complete_fields(rec) -> list:
    if rec.get("stage") == "explore":
        return ["raw_outcome", "exit_code", "search_wall_ms", "states_explored",
                "transitions_explored", "report_complete", "wall_ms"]
    return ["raw_outcome", "exit_code", "artifact_path", "artifact_sha256",
            "search_wall_ms", "verification_calls", "states_explored", "patch_len",
            "root_outcome", "wall_ms", "replay_ok", "replay_exit"]


def missing_complete_fields(rec) -> list:
    missing = [f for f in required_complete_fields(rec)
               if f not in rec or rec.get(f) is None]
    if rec.get("stage") != "explore":
        if rec.get("replay_ok") is not True:
            missing.append("replay_ok!=true")
        if rec.get("replay_exit") != 0:
            missing.append("replay_exit!=0")
    return missing


def evidence_reference_errors(rec) -> list:
    """Cheap checks that a complete/pending record still has valid evidence.

    No backend/replay is executed; only the referenced files and the record's
    own binding are checked. Non-evidence statuses are valid observations.
    """
    st = rec.get("evidence_status")
    if st not in ("complete", "replay_pending"):
        return []
    errs = []
    if st == "complete":
        errs += [f"missing:{m}" for m in missing_complete_fields(rec)]
    if rec.get("stage") == "explore":
        if st == "complete" and EXPLORE_EXIT.get(rec.get("raw_outcome")) != rec.get("exit_code"):
            errs.append("exit-mismatch")
        return errs
    art = rec.get("artifact_path")
    if not art:
        errs.append("artifact-path-missing")
    else:
        p = Path(art)
        if not p.exists():
            errs.append("artifact-missing")
        else:
            if not rec.get("artifact_sha256"):
                errs.append("artifact-hash-missing")
            elif sha256_file(p) != rec["artifact_sha256"]:
                errs.append("artifact-hash-mismatch")
            try:
                a = json.loads(p.read_text())
                if a.get("outcome") != rec.get("raw_outcome"):
                    errs.append("artifact-outcome-mismatch")
            except Exception:
                errs.append("artifact-json")
    if st == "complete" and REPAIR_EXIT.get(rec.get("raw_outcome")) != rec.get("exit_code"):
        errs.append("exit-mismatch")
    if st == "complete" and rec.get("replay_ok") is not True:
        errs.append("replay-not-ok")
    return errs


def is_valid_complete(rec) -> bool:
    return rec.get("evidence_status") == "complete" and not evidence_reference_errors(rec)


def sha256_file(p: Path) -> str:
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


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


def determinism_key(r):
    """Full experiment identity without repeat; strategy is kept (repeats of the
    same strategy are compared), config values and timeouts are part of it."""
    return sha_canonical({
        "stage": r.get("stage"), "suite": r.get("suite"), "case": r.get("case"),
        "config_name": r.get("config_name"), "config": r.get("config"),
        "strategy": r.get("strategy"), "identity": r.get("identity"),
        "engine": r.get("engine"), "search_timeout": r.get("search_timeout"),
        "replay_timeout": r.get("replay_timeout"),
    })


def pair_key(r):
    """Experiment identity without strategy (pairing differs only in strategy)
    but with repeat (B and C of the same repeat)."""
    return sha_canonical({
        "stage": r.get("stage"), "suite": r.get("suite"), "case": r.get("case"),
        "config_name": r.get("config_name"), "config": r.get("config"),
        "repeat": r.get("repeat"), "identity": r.get("identity"),
        "engine": r.get("engine"), "search_timeout": r.get("search_timeout"),
        "replay_timeout": r.get("replay_timeout"),
    })[:16]


def nf(r):
    return {k: r.get(k) for k in NON_TIME_FIELDS}


def _fmt(v):
    return "" if v is None else str(v)


def _search_ms(rec):
    return rec.get("search_wall_ms")


def _cumulative_attempt_ms(batch_dir: Path):
    """Sum the wall time of the stages each attempt actually executed.

    A normal attempt contributes its search time plus its replay time; a
    replay-only recovery contributes only its replay time (it inherited the
    search from the attempt it recovered and must not count that search again).
    This auxiliary ledger is never used for strategy comparison, which uses
    `search_wall_ms`.
    """
    out = defaultdict(int)
    path = batch_dir / "attempts.jsonl"
    if path.exists():
        for line in path.read_text().splitlines():
            if not line.strip():
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            replay_only = r.get("recovered_from_attempt") is not None
            search = 0 if replay_only else int(r.get("search_wall_ms") or 0)
            replay = int(r.get("replay_wall_ms") or 0)
            out[r.get("run_key")] += search + replay
    return out


def summarize_batch(batch_dir: Path, out: Path) -> int:
    recs = load_index(batch_dir)
    meta = load_json(batch_dir / "batch.json", {})
    plan = load_json(batch_dir / "plan.json", [])
    witness = load_json(out / "witness_results.json", [])
    env = load_json(batch_dir / "environment.json", {})
    cumulative = _cumulative_attempt_ms(batch_dir)

    errors = []

    # ---- index / plan consistency ----
    keys = [r.get("run_key") for r in recs]
    if len(keys) != len(set(keys)):
        dups = sorted({k for k in keys if keys.count(k) > 1})
        errors.append(f"duplicate run_key in valid index: {dups}")
    if plan:
        plan_keys = {p["run_key"] for p in plan}
        idx_keys = set(keys)
        unknown = sorted(idx_keys - plan_keys)
        missing = sorted(plan_keys - idx_keys)
        if unknown:
            errors.append(f"valid index has keys not in the frozen plan: {unknown}")
        if missing:
            errors.append(f"frozen plan has runs missing from the valid index: {missing}")

    # ---- evidence validity (file/binding only, never a backend re-run) ----
    for r in recs:
        ref_errs = evidence_reference_errors(r)
        if ref_errs:
            errors.append(
                f"{r.get('evidence_status')} record {r.get('case')}/{r.get('strategy')}/"
                f"{r.get('config_name')}/r{r.get('repeat')} failed evidence check: {ref_errs}")
        if r.get("evidence_status") == "evidence_invalid":
            errors.append(
                f"indexed evidence_invalid record {r.get('case')}/{r.get('strategy')}/"
                f"{r.get('config_name')}/r{r.get('repeat')}: {r.get('evidence_errors')}")

    complete = [r for r in recs if is_valid_complete(r)]
    valid_complete_keys = {r["run_key"] for r in complete}
    not_complete = [r for r in recs if r.get("run_key") not in valid_complete_keys]

    # ---- flat CSV ----
    fields = [
        "stage", "suite", "case", "family", "config_name", "strategy", "repeat",
        "raw_outcome", "final_classification", "evidence_status", "stop_reason", "truncation",
        "saw_unknown", "root_outcome", "report_complete", "proposals", "unique_programs",
        "verification_calls", "cache_hits", "states_explored", "transitions_explored", "nodes",
        "patch_len", "accepted_chain_len", "exit_code", "wall_ms", "wall_ms_basis",
        "search_wall_ms", "replay_exit", "replay_wall_ms", "end_to_end_wall_ms",
        "cumulative_attempt_wall_ms", "replay_ok", "artifact_sha256",
        "artifact_path", "attempt_id", "reused_from",
    ]
    with (batch_dir / "summary.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for r in sorted(recs, key=lambda r: (r.get("suite", ""), r.get("case", ""),
                                             r.get("config_name", ""), r.get("strategy", ""),
                                             r.get("repeat", 0), r.get("stage", ""))):
            row = dict(r)
            row["cumulative_attempt_wall_ms"] = cumulative.get(r.get("run_key"))
            w.writerow(row)

    # ---- determinism across repeats (full identity, repeat excluded) ----
    groups = defaultdict(list)
    for r in complete:
        if r.get("stage") == "repair":
            groups[determinism_key(r)].append(r)
    nondeterministic = []
    wall_stats = []
    for key, rs in sorted(groups.items()):
        repeats = [r.get("repeat") for r in rs]
        if len(repeats) != len(set(repeats)):
            errors.append(f"duplicate repeat inside one identity group {rs[0].get('case')}/"
                          f"{rs[0].get('config_name')}/{rs[0].get('strategy')}: {repeats}")
        if len(rs) < 2:
            continue
        base = nf(rs[0])
        if any(nf(r) != base for r in rs[1:]):
            nondeterministic.append([rs[0].get("case"), rs[0].get("config_name"),
                                     rs[0].get("strategy")])
        ws = [r["search_wall_ms"] for r in rs]
        wall_stats.append({"case": rs[0].get("case"), "config": rs[0].get("config_name"),
                           "strategy": rs[0].get("strategy"), "repeats": len(rs),
                           "cost_field": "search_wall_ms",
                           "min_ms": min(ws), "max_ms": max(ws),
                           "mean_ms": round(sum(ws) / len(ws), 1)})

    # ---- outcome pairing over the whole plan, keeping every outcome ----
    pair_groups = defaultdict(dict)
    for r in recs:
        if r.get("stage") == "repair" and r.get("strategy") in ("a", "b", "c"):
            pair_groups[pair_key(r)][r["strategy"]] = r
    planned_pair_keys = set()
    for p in plan:
        if p.get("stage") == "repair" and p.get("strategy") in ("b", "c"):
            planned_pair_keys.add(pair_key(p))

    def outcome(rec):
        return None if rec is None else rec.get("final_classification")

    paired = []
    for g, d in sorted(pair_groups.items()):
        a, b, c = d.get("a"), d.get("b"), d.get("c")
        b_ok = b is not None and b.get("final_classification") == "repaired"
        c_ok = c is not None and c.get("final_classification") == "repaired"
        present = [s for s, r in (("a", a), ("b", b), ("c", c)) if r is not None]
        not_executed = [s for s, r in (("a", a), ("b", b), ("c", c))
                        if r is not None and r.get("evidence_status") == "not_executed"]
        invalid = [s for s, r in (("a", a), ("b", b), ("c", c))
                   if r is not None and r.get("evidence_status") in FAULT_STATUSES]
        b_present, c_present = b is not None, c is not None
        b_attempted = b_present and b.get("evidence_status") != "not_executed"
        c_attempted = c_present and c.get("evidence_status") != "not_executed"
        both_present = b_present and c_present
        both_attempted = b_attempted and c_attempted
        both_complete = is_valid_complete(b) and is_valid_complete(c) if both_attempted else False
        row = {
            "pair_group": g,
            "case": (b or c or a).get("case"),
            "config_name": (b or c or a).get("config_name"),
            "repeat": (b or c or a).get("repeat"),
            "a": outcome(a), "b": outcome(b), "c": outcome(c),
            "a_status": None if a is None else a.get("evidence_status"),
            "b_status": None if b is None else b.get("evidence_status"),
            "c_status": None if c is None else c.get("evidence_status"),
            "strategies_present": present,
            "both_present": both_present,
            "both_attempted": both_attempted,
            "both_complete": both_complete,
            "both_repaired": b_ok and c_ok,
            "success_differs": both_attempted and (b_ok != c_ok),
            "repaired_vs_not_executed": (
                (b_ok and c_present and c.get("evidence_status") == "not_executed")
                or (c_ok and b_present and b.get("evidence_status") == "not_executed")
            ),
            "not_executed_sides": not_executed,
            "invalid_sides": invalid,
            "b_search_wall_ms": None if b is None else _search_ms(b),
            "c_search_wall_ms": None if c is None else _search_ms(c),
            "b_replay_wall_ms": None if b is None else b.get("replay_wall_ms"),
            "c_replay_wall_ms": None if c is None else c.get("replay_wall_ms"),
            "b_end_to_end_wall_ms": None if b is None else b.get("end_to_end_wall_ms"),
            "c_end_to_end_wall_ms": None if c is None else c.get("end_to_end_wall_ms"),
            "b_cumulative_wall_ms": None if b is None else cumulative.get(b.get("run_key")),
            "c_cumulative_wall_ms": None if c is None else cumulative.get(c.get("run_key")),
            "b_ver": None if b is None else b.get("verification_calls"),
            "c_ver": None if c is None else c.get("verification_calls"),
            "b_states": None if b is None else b.get("states_explored"),
            "c_states": None if c is None else c.get("states_explored"),
        }
        paired.append(row)

    attempted = [p for p in paired if p["both_attempted"]]
    both = [p for p in paired if p["both_repaired"]]
    outcome_differs = [p for p in paired if p["success_differs"]]
    paired_summary = {
        "planned_pairs": len(planned_pair_keys),
        "index_pairs": len(paired),
        "records_present": sum(1 for p in paired if p["both_present"]),
        "attempted_pairs": len(attempted),
        "both_complete": sum(1 for p in paired if p["both_complete"]),
        "both_repaired": len(both),
        "one_side_not_executed": sum(1 for p in paired if p["not_executed_sides"]),
        "repaired_vs_not_executed": sum(1 for p in paired if p["repaired_vs_not_executed"]),
        "one_side_invalid": sum(1 for p in paired if p["invalid_sides"]),
        "success_differs_count": len(outcome_differs),
        "outcome_differences": outcome_differs,
        # cost comparison subset: only both-repaired, search cost only
        "cost_subset_pairs": len(both),
        "cost_field": "search_wall_ms",
        "c_le_b_ver": sum(1 for p in both if p["c_ver"] <= p["b_ver"]),
        "c_lt_b_ver": sum(1 for p in both if p["c_ver"] < p["b_ver"]),
        "sum_b_ver": sum(p["b_ver"] for p in both),
        "sum_c_ver": sum(p["c_ver"] for p in both),
        "sum_b_states": sum(p["b_states"] for p in both),
        "sum_c_states": sum(p["c_states"] for p in both),
        "sum_b_search_wall_ms": sum(p["b_search_wall_ms"] for p in both),
        "sum_c_search_wall_ms": sum(p["c_search_wall_ms"] for p in both),
        "note": "pairs include repeats/configs of the same model, not independent programs; "
                "timeout/UNKNOWN/failure outcomes are kept",
    }

    # ---- denominators ----
    by_suite = defaultdict(Counter)
    status_by_suite = defaultdict(Counter)
    for r in recs:
        by_suite[r.get("suite", "?")][r.get("final_classification", "?")] += 1
        status_by_suite[r.get("suite", "?")][r.get("evidence_status", "?")] += 1

    # ---- matrix (explore) table: evidence status vs report completeness ----
    matrix = []
    for r in recs:
        if r.get("stage") == "explore":
            matrix.append({
                "case": r["case"], "bounds_variant": r["config_name"],
                "bounds": (r.get("identity", {}) or {}).get("contract_bounds", {}),
                "outcome": r.get("raw_outcome"), "evidence_status": r.get("evidence_status"),
                "report_complete": r.get("report_complete"),
                "states": r.get("states_explored"), "transitions": r.get("transitions_explored"),
                "search_wall_ms": r.get("search_wall_ms"),
            })

    summary = {
        "batch_id": meta.get("batch_id"),
        "cohort_fingerprint": meta.get("cohort_fingerprint"),
        "records": len(recs),
        "complete": len(complete),
        "not_complete": len(not_complete),
        "errors": errors,
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
    (batch_dir / "tables.md").write_text(build_tables(recs, summary, witness))
    print(json.dumps({
        "records": len(recs), "complete": len(complete), "not_complete": len(not_complete),
        "errors": errors, "nondeterministic": nondeterministic,
        "planned_pairs": paired_summary["planned_pairs"],
        "attempted_pairs": paired_summary["attempted_pairs"],
        "both_repaired": paired_summary["both_repaired"],
        "success_differs": paired_summary["success_differs_count"],
    }, indent=2))
    return 1 if errors else 0


def build_tables(recs, summary, witness) -> str:
    L = []
    meta = summary.get("meta", {})
    L.append("# Pilot generated tables\n")
    L.append(f"- batch_id: `{meta.get('batch_id')}`")
    L.append(f"- cohort_fingerprint: `{meta.get('cohort_fingerprint')}`")
    L.append(f"- binary_sha256: `{meta.get('binary_sha256')}`")
    L.append(f"- code_fingerprint: `{meta.get('code_fingerprint')}`")
    L.append(f"- records: {summary['records']} (complete {summary['complete']}, "
             f"other {summary['not_complete']})")
    if summary.get("errors"):
        L.append("")
        L.append("**Summary errors (invalid or incomplete evidence):**")
        for e in summary["errors"]:
            L.append(f"- {e}")
    L.append("")

    # per-case repair table (all configs present in the batch), A/B/C
    L.append("## Repair results (configs present in this batch)\n")
    L.append("`vN` verification calls, `sN` cumulative states, `tN` = search_wall_ms "
             "(search cost only; replay and end-to-end are separate columns).\n")
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
                           and r["repeat"] == 1]
                if not matches:
                    cells.append("")
                    continue
                r = matches[0]
                if not is_valid_complete(r):
                    cells.append(f"{r.get('final_classification')}")
                else:
                    cells.append(f"{r.get('final_classification')} v{r.get('verification_calls')} "
                                 f"s{r.get('states_explored')} t{r.get('search_wall_ms')}")
            L.append(f"| {case} | {config} | {cells[0]} | {cells[1]} | {cells[2]} |")
    L.append("")

    # paired B/C outcome summary (all outcomes kept)
    ps = summary["paired"]
    L.append("## Strategy pairing (all outcomes kept)\n")
    L.append(f"- planned B/C pairs: {ps['planned_pairs']}; pairs in index: {ps['index_pairs']}; "
             f"both attempted: {ps['attempted_pairs']}")
    L.append(f"- both evidence-complete: {ps['both_complete']}; both repaired: {ps['both_repaired']}")
    L.append(f"- pairs with a not_executed side: {ps['one_side_not_executed']}; "
             f"pairs with an invalid/limited side: {ps['one_side_invalid']}; "
             f"outcome differences: {ps['success_differs_count']}")
    if ps["outcome_differences"]:
        L.append("\n### Outcome differences (B vs C)\n")
        L.append("| case | config | repeat | A | B | C |")
        L.append("| --- | --- | --- | --- | --- | --- |")
        for p in ps["outcome_differences"]:
            L.append(f"| {p['case']} | {p['config_name']} | {p['repeat']} | {_fmt(p['a'])} | "
                     f"{_fmt(p['b'])} | {_fmt(p['c'])} |")
    L.append("")
    L.append("### Cost comparison subset: both repaired, `search_wall_ms` only\n")
    L.append(f"- subset pairs: {ps['cost_subset_pairs']}; C<=B verification: "
             f"{ps['c_le_b_ver']}/{ps['both_repaired']}; C<B: {ps['c_lt_b_ver']}")
    L.append(f"- sum verification B/C = {ps['sum_b_ver']}/{ps['sum_c_ver']}; "
             f"sum states B/C = {ps['sum_b_states']}/{ps['sum_c_states']}; "
             f"sum search wall B/C = {ps['sum_b_search_wall_ms']}/{ps['sum_c_search_wall_ms']} ms")
    L.append(f"- {ps['note']}")
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
    L.append("## Determinism (full identity, repeat excluded; `search_wall_ms`)\n")
    if summary["nondeterministic"]:
        L.append("Nondeterministic groups: " + ", ".join(str(g) for g in summary["nondeterministic"]))
    else:
        L.append("No nondeterministic groups over all non-time fields.")
    L.append("")
    L.append("| case | config | strategy | repeats | min_ms | max_ms | mean_ms |")
    L.append("| --- | --- | --- | --- | --- | --- | --- |")
    for w in summary["wall_stats"]:
        L.append(f"| {w['case']} | {w['config']} | {w['strategy']} | {w['repeats']} | "
                 f"{w['min_ms']} | {w['max_ms']} | {w['mean_ms']} |")
    L.append("")

    # matrix
    if summary["matrix"]:
        L.append("## Root verification budget matrix (explore only, no search)\n")
        L.append("`evidence_status=complete` means the record is well-formed; "
                 "`report_complete` is the backend's own bounded-exploration flag.\n")
        L.append("| case | variant | bounds.max_states | outcome | evidence_status | "
                 "report_complete | states | transitions | search_wall_ms |")
        L.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
        for m in summary["matrix"]:
            b = m.get("bounds") or {}
            L.append(f"| {m['case']} | {m['bounds_variant']} | {b.get('max_states','')} | "
                     f"{m['outcome']} | {m['evidence_status']} | {_fmt(m['report_complete'])} | "
                     f"{_fmt(m['states'])} | {_fmt(m['transitions'])} | {m['search_wall_ms']} |")
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
