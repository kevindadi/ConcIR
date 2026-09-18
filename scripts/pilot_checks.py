#!/usr/bin/env python3
"""Post-hoc checks for the pilot statistics path.

Runs the real `pilot_analyze.summarize_batch` on synthetic batch directories and
confirms:

* it reads only the unique `valid_index.jsonl` (never `attempts.jsonl`), so
  duplicate attempts cannot inflate denominators;
* a `complete` record with a missing field is an error, not zero;
* a `complete` record whose referenced artifact is missing or whose hash does
  not match is an error and is not counted as a success.

Separate from the runner so the run-time experiment code identity stays frozen.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))
import pilot_analyze  # noqa: E402


def main():
    d = Path(tempfile.mkdtemp(prefix="concir-summarize-check-"))
    try:
        art = d / "art.json"
        art.write_text(json.dumps({"outcome": "repaired"}))
        ahash = hashlib.sha256(art.read_bytes()).hexdigest()

        def complete_record(**over):
            r = {
                "run_key": "k1", "stage": "repair", "suite": "pilot", "case": "p1_same",
                "config_name": "main", "config": {"candidate_budget": 64},
                "strategy": "b", "repeat": 1, "identity": {"model_norm_sha256": "m"},
                "evidence_status": "complete", "final_classification": "repaired",
                "raw_outcome": "repaired", "exit_code": 0,
                "artifact_path": str(art), "artifact_sha256": ahash,
                "search_wall_ms": 8, "verification_calls": 2, "states_explored": 92,
                "patch_len": 1, "root_outcome": "FAIL", "wall_ms": 8,
                "replay_ok": True, "replay_exit": 0, "search_timeout": 30.0, "replay_timeout": 30.0,
            }
            r.update(over)
            return r

        # attempts log: two attempts for one logical run; only the valid index counts.
        attempts = [dict(complete_record(), final_classification="replay_failed",
                         evidence_status="replay_failed", raw_outcome=None),
                    complete_record()]
        (d / "attempts.jsonl").write_text("\n".join(json.dumps(a) for a in attempts) + "\n")
        (d / "plan.json").write_text(json.dumps([{"run_key": "k1"}]))
        (d / "valid_index.jsonl").write_text(json.dumps(complete_record()) + "\n")
        (d / "batch.json").write_text(json.dumps({"batch_id": "synthetic"}) + "\n")
        rc = pilot_analyze.summarize_batch(d, d)
        s = json.loads((d / "summary.json").read_text())
        ok = rc == 0 and s["records"] == 1 and s["complete"] == 1 and not s["errors"]

        # missing required field -> error, not zero
        bad = dict(complete_record())
        del bad["verification_calls"]
        (d / "valid_index.jsonl").write_text(json.dumps(bad) + "\n")
        rc_bad = pilot_analyze.summarize_batch(d, d)
        s_bad = json.loads((d / "summary.json").read_text())
        ok_bad = rc_bad != 0 and any("verification_calls" in e for e in s_bad["errors"])

        # artifact missing / hash mismatch -> error, not a success
        missing = complete_record(run_key="k2", artifact_path=str(d / "nope.json"))
        mismatch = complete_record(run_key="k3", artifact_sha256="0" * 64)
        (d / "plan.json").write_text(json.dumps([{"run_key": "k2"}, {"run_key": "k3"}]))
        (d / "valid_index.jsonl").write_text(json.dumps(missing) + "\n" + json.dumps(mismatch) + "\n")
        rc_ref = pilot_analyze.summarize_batch(d, d)
        s_ref = json.loads((d / "summary.json").read_text())
        ok_ref = (rc_ref != 0 and s_ref["complete"] == 0
                  and any("artifact-missing" in e for e in s_ref["errors"])
                  and any("artifact-hash-mismatch" in e for e in s_ref["errors"]))

        result = ok and ok_bad and ok_ref
        print(f"{'PASS' if ok else 'FAIL'}  summarize_reads_valid_index_only: "
              f"records={s['records']} complete={s['complete']} (attempts={len(attempts)})")
        print(f"{'PASS' if ok_bad else 'FAIL'}  summarize_flags_missing_evidence: "
              f"rc={rc_bad} errors={s_bad['errors']}")
        print(f"{'PASS' if ok_ref else 'FAIL'}  summarize_rejects_invalid_references: "
              f"rc={rc_ref} complete={s_ref['complete']} errors={s_ref['errors']}")
        out = REPO / "experiments" / "pilot-v2-lifecycle"
        out.mkdir(parents=True, exist_ok=True)
        (out / "summarize_check.txt").write_text(
            f"{'PASS' if ok else 'FAIL'} summarize_reads_valid_index_only records={s['records']}\n"
            f"{'PASS' if ok_bad else 'FAIL'} summarize_flags_missing_evidence rc={rc_bad}\n"
            f"{'PASS' if ok_ref else 'FAIL'} summarize_rejects_invalid_references rc={rc_ref}\n")
        return 0 if result else 1
    finally:
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
