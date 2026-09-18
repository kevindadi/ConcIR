#!/usr/bin/env python3
"""Independent audit of a pilot results tree.

Re-hashes every artifact referenced by a valid-run index, checks the recorded
counts/exit codes against the artifact, and confirms that every `complete`
repair was replayed. A `complete` record missing a required evidence field is an
issue, never silently treated as zero. Read-only.

Usage: python3 scripts/pilot_audit.py [experiments/pilot-v2]
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

REPAIR_EXIT = {
    "repaired": 0, "already_satisfied": 0, "no_acceptable_candidate": 1,
    "budget_exhausted": 1, "analysis_unknown": 3, "invalid": 4,
    "invalid_config": 4, "unsupported": 5,
}


def required_complete_fields(rec) -> list:
    if rec.get("stage") == "explore":
        return ["raw_outcome", "exit_code", "search_wall_ms", "states_explored",
                "transitions_explored", "report_complete", "wall_ms"]
    return ["raw_outcome", "exit_code", "artifact_path", "artifact_sha256",
            "search_wall_ms", "verification_calls", "states_explored", "patch_len",
            "root_outcome", "wall_ms", "replay_ok", "replay_exit"]


def sha256_file(p: Path) -> str:
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def audit(root: Path):
    batches = root / "results" / "batches"
    out = {"batches": [], "issues": []}
    if not batches.exists():
        return out
    for bdir in sorted(batches.iterdir()):
        idx = bdir / "valid_index.jsonl"
        if not idx.exists():
            continue
        recs = [json.loads(l) for l in idx.read_text().splitlines() if l.strip()]
        keys = [r.get("run_key") for r in recs]
        b = {
            "batch_id": bdir.name,
            "records": len(recs),
            "unique_run_keys": len(set(keys)),
            "complete": 0,
            "not_complete": 0,
            "artifacts_hashed": 0,
            "artifact_hash_mismatch": 0,
            "count_mismatch": 0,
            "exit_mismatch": 0,
            "replay_missing": 0,
            "incomplete_fields": 0,
            "classification": {},
        }
        if len(keys) != len(set(keys)):
            out["issues"].append(f"{bdir.name}: duplicate run_key")
        for r in recs:
            b["classification"][r.get("final_classification")] = \
                b["classification"].get(r.get("final_classification"), 0) + 1
            if r.get("evidence_status") != "complete":
                b["not_complete"] += 1
                continue
            missing = [f for f in required_complete_fields(r)
                       if f not in r or r.get(f) is None]
            if r.get("stage") != "explore":
                if r.get("replay_ok") is not True:
                    missing.append("replay_ok!=true")
                if r.get("replay_exit") != 0:
                    missing.append("replay_exit!=0")
            if missing:
                b["incomplete_fields"] += 1
                out["issues"].append(
                    f"{bdir.name}/{r.get('case')}/{r.get('strategy')}: complete but missing {missing}")
                continue
            b["complete"] += 1
            if r.get("stage") != "repair":
                continue
            art = r.get("artifact_path")
            if not art or not Path(art).exists():
                out["issues"].append(f"{bdir.name}: complete record without artifact {art}")
                b["replay_missing"] += 1
                continue
            p = Path(art)
            h = sha256_file(p)
            b["artifacts_hashed"] += 1
            if h != r["artifact_sha256"]:
                b["artifact_hash_mismatch"] += 1
                out["issues"].append(f"{bdir.name}/{r['case']}/{r['strategy']}: artifact hash")
            a = json.loads(p.read_text())
            c = a.get("counts", {})
            for rec_key, art_key in [("proposals", "proposals"),
                                     ("unique_programs", "unique_candidate_programs"),
                                     ("verification_calls", "verification_calls"),
                                     ("cache_hits", "cache_hits"),
                                     ("states_explored", "states_explored"),
                                     ("nodes", "nodes")]:
                if r.get(rec_key) != c.get(art_key):
                    b["count_mismatch"] += 1
                    out["issues"].append(
                        f"{bdir.name}/{r['case']}/{r['strategy']}: {rec_key} "
                        f"{r.get(rec_key)} != artifact {c.get(art_key)}")
            if a.get("outcome") != r.get("raw_outcome"):
                b["count_mismatch"] += 1
                out["issues"].append(f"{bdir.name}/{r['case']}/{r['strategy']}: outcome")
            if REPAIR_EXIT.get(r.get("raw_outcome")) != r.get("exit_code"):
                b["exit_mismatch"] += 1
                out["issues"].append(f"{bdir.name}/{r['case']}/{r['strategy']}: exit mapping")
        out["batches"].append(b)
    return out


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("experiments/pilot-v2")
    result = audit(root)
    (root / "audit.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    raise SystemExit(1 if result["issues"] else 0)
