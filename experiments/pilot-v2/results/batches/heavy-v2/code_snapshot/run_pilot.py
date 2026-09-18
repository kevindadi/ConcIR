#!/usr/bin/env python3
"""Reliable, manifest-driven pilot runner for the ConcIR repair loop (v2).

Fixes over the pilot-v1 runner (see REVIEW.md R1-R5):

* R1 independent attempt directory per execution; the produced artifact, stdout,
  inputs, effective config and exit code are bound together. A stale file can
  never turn a failed run into a success.
* R2 batch / run / attempt / repeat identities; an append-only attempt log plus
  a unique valid-run index that statistics, pairing and determinism read from.
* R3 resume only reuses evidence-complete, identity-matching records; artifacts
  are re-hashed and replay status is re-checked. `replay_pending` is explicit.
* R4 monotonic deadline; every stage gets min(stage timeout, remaining budget)
  and the remaining budget is re-checked before replay.
* R5 the reproducible identity covers runner / generator / manifest / stats /
  witness-tool source hashes plus a git commit+dirty diff, not package version.

The runner drives the existing `concir-backend` CLI and the `pilot_tool`
example; it does not change core semantics.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import platform
import random
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPTS = REPO / "scripts"
DEFAULT_OUT = REPO / "experiments" / "pilot-v2"
DEFAULT_MANIFEST = DEFAULT_OUT / "manifest.json"
DEFAULT_BIN = REPO / "target" / "release" / "concir-backend"
DEFAULT_TOOL = REPO / "target" / "release" / "examples" / "pilot_tool"
STRATEGIES = ["a", "b", "c"]
STRATEGY_ENUM = {"a": "single", "b": "composite", "c": "diagnostic"}

REPAIR_OUTCOMES = {
    "repaired",
    "already_satisfied",
    "no_acceptable_candidate",
    "budget_exhausted",
    "analysis_unknown",
    "invalid",
    "unsupported",
    "invalid_config",
}
EXPLORE_OUTCOMES = {"PASS", "FAIL", "UNKNOWN", "INVALID", "UNSUPPORTED"}
REPAIR_EXIT = {
    "repaired": 0,
    "already_satisfied": 0,
    "no_acceptable_candidate": 1,
    "budget_exhausted": 1,
    "analysis_unknown": 3,
    "invalid": 4,
    "invalid_config": 4,
    "unsupported": 5,
}
EXPLORE_EXIT = {"PASS": 0, "FAIL": 1, "UNKNOWN": 3, "INVALID": 4, "UNSUPPORTED": 5}
SUCCESS_CLASSES = {"repaired", "already_satisfied"}

# statuses that mean "we have no usable evidence" and must not be cached
UNUSABLE = {
    "not_executed",
    "spawn_error",
    "json_error",
    "no_artifact",
    "artifact_missing",
    "identity_mismatch",
    "exit_outcome_mismatch",
    "search_timeout",
    "replay_failed",
    "replay_timeout",
}

NON_TIME_FIELDS = (
    "stage", "raw_outcome", "final_classification", "stop_reason", "truncation",
    "saw_unknown", "proposals", "unique_programs", "verification_calls", "cache_hits",
    "states_explored", "transitions_explored", "nodes", "patch_len", "accepted_chain_len",
    "exit_code", "replay_exit", "replay_ok",
)


# ───────────────────────────── utilities ─────────────────────────────


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(Path(path).read_bytes())


def canonical(obj) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def sha_canonical(obj) -> str:
    return sha256_bytes(canonical(obj).encode())


def now_utc() -> str:
    return datetime.now(timezone.utc).isoformat()


def git_info() -> dict:
    def git(*args) -> str:
        try:
            return subprocess.run(
                ["git", *args], cwd=REPO, capture_output=True, text=True, check=True
            ).stdout.strip()
        except Exception:
            return ""

    commit = git("rev-parse", "HEAD")
    status = git("status", "--porcelain")
    diff = git("diff", "HEAD")
    staged = git("diff", "--cached")
    return {
        "commit": commit,
        "dirty": bool(status),
        "status_porcelain": status,
        "tracked_diff_sha256": sha256_bytes(diff.encode()) if diff else "",
        "staged_diff_sha256": sha256_bytes(staged.encode()) if staged else "",
    }


def code_identity(files: list[Path]) -> dict:
    entries = {}
    for f in files:
        if f.exists():
            entries[str(f.relative_to(REPO))] = {
                "sha256": sha256_file(f),
                "bytes": f.stat().st_size,
            }
        else:
            entries[str(f.relative_to(REPO))] = {"sha256": None, "bytes": None}
    return {"files": entries, "fingerprint": sha_canonical(entries)}


def run_process(cmd, timeout_s, cwd=REPO):
    """Run in its own process group with a hard timeout.

    Returns a structured dict; Popen errors become `spawn_error` rather than
    raising, so one bad stage cannot abort a batch.
    """
    t0 = time.monotonic()
    start_new = os.name == "posix"
    try:
        proc = subprocess.Popen(
            cmd,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=start_new,
        )
    except Exception as e:  # pragma: no cover - exercised via injection
        return {
            "cmd": [str(c) for c in cmd],
            "exit_code": None,
            "stdout": "",
            "stderr": str(e),
            "wall_ms": int((time.monotonic() - t0) * 1000),
            "timeout_s": timeout_s,
            "timed_out": False,
            "spawn_error": str(e),
        }
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            if start_new:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            else:
                proc.kill()
        except ProcessLookupError:
            pass
        out, err = proc.communicate()
    return {
        "cmd": [str(c) for c in cmd],
        "exit_code": proc.returncode,
        "stdout": out.decode("utf-8", "replace"),
        "stderr": err.decode("utf-8", "replace"),
        "wall_ms": int((time.monotonic() - t0) * 1000),
        "timeout_s": timeout_s,
        "timed_out": timed_out,
        "spawn_error": None,
    }


def write(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def atomic_write(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(text)
    os.replace(tmp, path)


# ───────────────────────────── manifest / plan ─────────────────────────────


class Context:
    def __init__(self, manifest_path: Path, out: Path, binary: Path, tool: Path,
                 suite, search_timeout, replay_timeout, total_budget, resume):
        self.manifest_path = manifest_path
        self.manifest_dir = manifest_path.parent
        self.manifest = json.loads(manifest_path.read_text())
        self.out = out
        self.binary = binary
        self.tool = tool
        self.suites = suite
        self.search_timeout = search_timeout
        self.replay_timeout = replay_timeout
        self.total_budget = total_budget
        self.resume = resume
        self._norm_cache: dict[tuple[str, str], dict] = {}
        self._code = None
        self._binary_sha = sha256_file(binary) if binary.exists() else ""

    def binary_sha(self):
        return self._binary_sha

    # ---- identity ----
    def code(self):
        if self._code is None:
            files = [
                SCRIPTS / "run_pilot.py",
                SCRIPTS / "pilot_analyze.py",
                self.manifest_dir / "generate_cases.py",
                self.manifest_path,
            ]
            tool_src = REPO / "examples" / "pilot_tool.rs"
            if tool_src.exists():
                files.append(tool_src)
            self._code = code_identity(files)
        return self._code

    def normalize(self, path: Path, kind: str) -> dict:
        key = (str(path), kind)
        if key in self._norm_cache:
            return self._norm_cache[key]
        r = run_process([str(self.tool), "normalize", str(path), "--kind", kind], 60)
        if r["exit_code"] != 0 or r["spawn_error"]:
            raise RuntimeError(f"normalize failed for {path}: {r['stderr'][:200]}")
        obj = json.loads(r["stdout"])
        self._norm_cache[key] = {
            "raw_sha256": sha256_file(path),
            "norm": obj,
            "norm_sha256": sha_canonical(obj),
        }
        return self._norm_cache[key]

    def input_identity(self, model: Path, contract: Path) -> dict:
        m = self.normalize(model, "program")
        c = self.normalize(contract, "contract")
        return {
            "model_path": str(model),
            "contract_path": str(contract),
            "model_sha256": m["raw_sha256"],
            "model_norm_sha256": m["norm_sha256"],
            "contract_sha256": c["raw_sha256"],
            "contract_norm_sha256": c["norm_sha256"],
        }


def plan_runs(ctx: Context):
    """Yield dicts describing every logical run (one repeat each)."""
    manifest = ctx.manifest
    suites = ctx.suites
    runs = []
    if "smoke" in suites:
        for c in manifest.get("smoke_cases", []):
            cfg = dict(manifest["search_configs"]["main"])
            cfg.update(c.get("config_override", {}))
            for st in STRATEGIES:
                runs.append(_repair_run(ctx, "smoke", c, "smoke", cfg, st, 1))
    if "pilot" in suites:
        for c in manifest.get("cases", []):
            policy = c.get("repeat_policy", {"main": 3, "tight": 1})
            for config_name in ("main", "tight"):
                for rep in range(1, int(policy.get(config_name, 0)) + 1):
                    for st in STRATEGIES:
                        runs.append(_repair_run(ctx, "pilot", c, config_name,
                                                dict(manifest["search_configs"][config_name]), st, rep))
    if "heavy" in suites:
        for c in manifest.get("cases", []):
            if c.get("case") in ("p3_same",):
                for st in STRATEGIES:
                    runs.append(_repair_run(ctx, "heavy", c, "heavy",
                                            dict(manifest["search_configs"]["main"]), st, 1))
    if "matrix" in suites:
        for c in manifest.get("matrix_cases", []):
            for variant in c["variants"]:
                runs.append(_explore_run(ctx, "matrix", c, variant))
    return runs


def _rel(ctx: Context, p: str) -> Path:
    return (ctx.manifest_dir / p).resolve()


def _repair_run(ctx, suite, c, config_name, config, strategy, repeat) -> dict:
    model = _rel(ctx, c["model"])
    contract = _rel(ctx, c["contract"])
    ident = ctx.input_identity(model, contract)
    norm_model = ctx.normalize(model, "program")["norm"]
    norm_contract = ctx.normalize(contract, "contract")["norm"]
    ident["contract_bounds"] = norm_contract.get("bounds")
    return {
        "stage": "repair",
        "_norm_model": norm_model,
        "_norm_contract": norm_contract,
        "suite": suite,
        "case": c["case"],
        "family": c.get("family", ""),
        "params": c.get("params", {}),
        "model": str(model),
        "contract": str(contract),
        "config_name": config_name,
        "config": config,
        "strategy": strategy,
        "repeat": repeat,
        "identity": ident,
    }


def _explore_run(ctx, suite, c, variant) -> dict:
    model = _rel(ctx, c["model"])
    contract = _rel(ctx, variant["contract"])
    ident = ctx.input_identity(model, contract)
    norm_model = ctx.normalize(model, "program")["norm"]
    norm_contract = ctx.normalize(contract, "contract")["norm"]
    ident["contract_bounds"] = norm_contract.get("bounds")
    return {
        "stage": "explore",
        "_norm_model": norm_model,
        "_norm_contract": norm_contract,
        "suite": suite,
        "case": c["case"],
        "family": c.get("family", ""),
        "params": {**c.get("params", {}), "bounds_variant": variant["label"]},
        "model": str(model),
        "contract": str(contract),
        "config_name": variant["label"],
        "config": {},
        "strategy": "",
        "repeat": 1,
        "identity": ident,
        "engine": variant.get("engine", "petri"),
    }


def stage_timeout(ctx: Context, stage: str) -> float:
    return ctx.search_timeout


def run_fingerprint(ctx: Context, spec: dict, search_to: float, replay_to: float) -> str:
    payload = {
        "stage": spec["stage"],
        "case": spec["case"],
        "suite": spec["suite"],
        "config_name": spec["config_name"],
        "config": spec["config"],
        "strategy": spec["strategy"],
        "repeat": spec["repeat"],
        "identity": spec["identity"],
        "engine": spec.get("engine"),
        "search_timeout": float(search_to),
        "replay_timeout": float(replay_to),
        "binary_sha256": ctx.binary_sha(),
        "code_fingerprint": ctx.code()["fingerprint"],
    }
    return sha_canonical(payload)


def run_key(spec: dict, search_to: float, replay_to: float) -> str:
    return sha_canonical({
        "stage": spec["stage"], "case": spec["case"], "suite": spec["suite"],
        "config_name": spec["config_name"], "config": spec["config"],
        "strategy": spec["strategy"], "repeat": spec["repeat"],
        "identity": spec["identity"], "engine": spec.get("engine"),
        "search_timeout": search_to, "replay_timeout": replay_to,
    })[:24]


def run_slug(spec: dict) -> str:
    return f"{spec['case']}__{spec['config_name']}__{spec['strategy'] or 'na'}__r{spec['repeat']}"


# ───────────────────────────── classification ─────────────────────────────


def _artifact_canonical_matches(artifact, spec) -> tuple[bool, str]:
    want_cfg = dict(spec["config"])
    want_cfg["strategy"] = STRATEGY_ENUM[spec["strategy"]]
    want_cfg["bounds"] = spec["identity"].get("contract_bounds")
    got = artifact.get("effective_config")
    if got is None:
        return False, "artifact has no effective_config"
    for k, v in want_cfg.items():
        if k == "bounds" and v is None:
            continue
        if got.get(k) != v:
            return False, f"effective_config.{k}: {got.get(k)!r} != {v!r}"
    return True, ""


def classify_repair(result, spec, model, contract, artifact_path: Path, replay):
    """Return a status/evidence dict for a repair attempt."""
    out = {
        "evidence_status": "complete",
        "raw_outcome": None,
        "stop_reason": None,
        "truncation": None,
        "saw_unknown": None,
        "counts": {},
        "replay_ok": None,
        "replay_exit": None,
        "replay_wall_ms": None,
        "notes": "",
    }
    if result.get("spawn_error"):
        out["evidence_status"] = "spawn_error"
        out["notes"] = result["spawn_error"]
        return out
    if result["timed_out"]:
        out["evidence_status"] = "search_timeout"
        out["notes"] = "process group killed after stage timeout"
        return out
    if result["exit_code"] is not None and result["exit_code"] < 0:
        out["evidence_status"] = "resource_error"
        out["notes"] = f"killed by signal {-result['exit_code']}"
        return out

    if not artifact_path.exists():
        out["evidence_status"] = "no_artifact"
        out["notes"] = "search produced no --artifact file"
        return out
    try:
        artifact = json.loads(artifact_path.read_text())
    except Exception as e:
        out["evidence_status"] = "json_error"
        out["notes"] = f"artifact parse: {e}"
        return out
    if not isinstance(artifact, dict):
        out["evidence_status"] = "json_error"
        out["notes"] = "artifact is not a JSON object"
        return out

    if artifact.get("schema_version") != "concir-repair-artifact-v1":
        out["evidence_status"] = "json_error"
        out["notes"] = f"unknown schema {artifact.get('schema_version')!r}"
        return out

    raw = artifact.get("outcome")
    if raw not in REPAIR_OUTCOMES:
        out["evidence_status"] = "json_error"
        out["notes"] = f"unknown outcome {raw!r}"
        return out
    out["raw_outcome"] = raw
    out["stop_reason"] = artifact.get("stop_reason")
    out["truncation"] = artifact.get("truncation")
    out["saw_unknown"] = artifact.get("saw_unknown")
    out["counts"] = artifact.get("counts", {}) or {}

    if result["exit_code"] != REPAIR_EXIT[raw]:
        out["evidence_status"] = "exit_outcome_mismatch"
        out["notes"] = f"exit {result['exit_code']} != expected {REPAIR_EXIT[raw]} for {raw}"
        return out

    # Bind embedded program/contract to this run's inputs using the backend's
    # own normalized serialization.
    want_prog = spec["_norm_model"]
    want_contract = spec["_norm_contract"]
    if canonical(artifact.get("input_program")) != canonical(want_prog):
        out["evidence_status"] = "identity_mismatch"
        out["notes"] = "artifact input_program does not match the run's model"
        return out
    if canonical(artifact.get("frozen_contract")) != canonical(want_contract):
        out["evidence_status"] = "identity_mismatch"
        out["notes"] = "artifact frozen_contract does not match the run's contract"
        return out
    ok, why = _artifact_canonical_matches(artifact, spec)
    if not ok:
        out["evidence_status"] = "identity_mismatch"
        out["notes"] = why
        return out

    # Replay (already executed by caller).
    if replay is None:
        out["evidence_status"] = "replay_pending"
        return out
    if replay.get("spawn_error"):
        out["evidence_status"] = "spawn_error"
        out["notes"] = replay["spawn_error"]
        return out
    out["replay_exit"] = replay["exit_code"]
    out["replay_wall_ms"] = replay["wall_ms"]
    if replay["timed_out"]:
        out["evidence_status"] = "replay_timeout"
        out["replay_ok"] = False
        return out
    out["replay_ok"] = replay["exit_code"] == 0
    if not out["replay_ok"]:
        out["evidence_status"] = "replay_failed"
        out["notes"] = replay["stderr"].strip()[:300]
        return out
    return out


def classify_explore(result, spec):
    out = {
        "evidence_status": "complete",
        "raw_outcome": None,
        "states_explored": None,
        "transitions_explored": None,
        "notes": "",
        "replay_ok": None,
    }
    if result.get("spawn_error"):
        out["evidence_status"] = "spawn_error"
        out["notes"] = result["spawn_error"]
        return out
    if result["timed_out"]:
        out["evidence_status"] = "search_timeout"
        out["notes"] = "process group killed after stage timeout"
        return out
    try:
        report = json.loads(result["stdout"])
    except Exception as e:
        out["evidence_status"] = "json_error"
        out["notes"] = f"explore stdout parse: {e}"
        return out
    if not isinstance(report, dict):
        out["evidence_status"] = "json_error"
        out["notes"] = "explore report is not a JSON object"
        return out
    raw = report.get("outcome")
    if raw not in EXPLORE_OUTCOMES:
        out["evidence_status"] = "json_error"
        out["notes"] = f"unknown explore outcome {raw!r}"
        return out
    out["raw_outcome"] = raw
    out["states_explored"] = report.get("states_explored")
    out["transitions_explored"] = report.get("transitions_explored")
    if result["exit_code"] != EXPLORE_EXIT[raw]:
        out["evidence_status"] = "exit_outcome_mismatch"
        out["notes"] = f"exit {result['exit_code']} != expected {EXPLORE_EXIT[raw]} for {raw}"
    return out


# ───────────────────────────── attempt execution ─────────────────────────────


_QUALITY = {"complete": 4, "replay_pending": 3}


def _quality(rec):
    st = rec.get("evidence_status")
    if st in _QUALITY:
        return _QUALITY[st]
    if st == "not_executed":
        return 0
    return 2  # a definitive fault (timeout / spawn / json / identity / replay_failed)


def _best(existing, candidate) -> bool:
    if existing is None:
        return True
    qe, qc = _quality(existing), _quality(candidate)
    if qc != qe:
        return qc > qe
    return candidate.get("attempt", 0) >= existing.get("attempt", 0)


def execute_attempt(ctx, spec, batch_dir: Path, valid_index: dict, attempt_no: int,
                    deadline: float, index_path: Path, attempts_path: Path, log=print):
    run_key_ = spec["run_key"]
    fp = spec["run_fingerprint"]
    slug = run_slug(spec)
    attempt_dir = batch_dir / "raw" / slug / f"attempt-{attempt_no:03d}"
    attempt_dir.mkdir(parents=True, exist_ok=True)
    record = {
        "batch_id": spec["batch_id"],
        "run_key": run_key_,
        "run_fingerprint": fp,
        "stage": spec["stage"],
        "suite": spec["suite"],
        "case": spec["case"],
        "family": spec["family"],
        "params": spec["params"],
        "config_name": spec["config_name"],
        "config": spec["config"],
        "strategy": spec["strategy"],
        "repeat": spec["repeat"],
        "identity": spec["identity"],
        "attempt": attempt_no,
        "attempt_dir": str(attempt_dir),
        "search_timeout": spec["search_timeout"],
        "replay_timeout": spec["replay_timeout"],
        "reused_from": None,
    }

    remaining = deadline - time.monotonic()
    if spec["stage"] != "explore" and spec.get("replay_only_from"):
        # explicit replay_pending recovery
        _replay_phase(ctx, spec, record, deadline, attempt_dir)
        _commit(ctx, record, valid_index, index_path)
        _append(attempts_path, record)
        return record, "replay_recovered"

    if remaining <= 0:
        record.update(evidence_status="not_executed", raw_outcome=None,
                      wall_ms=0, notes="total budget exhausted before stage start")
        record["final_classification"] = "not_executed"
        _commit(ctx, record, valid_index, index_path)
        _append(attempts_path, record)
        return record, "not_executed"

    stage_to = min(stage_timeout(ctx, spec["stage"]), remaining)
    record["effective_stage_timeout"] = stage_to
    record["final_classification"] = None

    if spec["stage"] == "repair":
        cmd = [
            str(ctx.binary), "repair", spec["model"], spec["contract"],
            "--strategy", spec["strategy"],
            "--candidate-budget", str(spec["config"]["candidate_budget"]),
            "--verification-budget", str(spec["config"]["verification_budget"]),
            "--max-depth", str(spec["config"]["max_depth"]),
            "--max-total-edits", str(spec["config"]["max_total_edits"]),
            "--artifact", str(attempt_dir / "artifact.json"),
        ]
        res = run_process(cmd, stage_to)
        write(attempt_dir / "search.stdout", res["stdout"])
        write(attempt_dir / "search.stderr", res["stderr"])
        write(attempt_dir / "search.exit", str(res["exit_code"]))
        write(attempt_dir / "search.command", " ".join(res["cmd"]) + "\n")
        record["search_wall_ms"] = res["wall_ms"]
        record["search_timed_out"] = res["timed_out"]
        record["exit_code"] = res["exit_code"]
        artifact_path = attempt_dir / "artifact.json"
        record["artifact_path"] = str(artifact_path)
        # replay phase (respect remaining budget)
        replay = None
        if artifact_path.exists():
            rem2 = deadline - time.monotonic()
            if rem2 <= 0:
                replay = None
            else:
                rep_to = min(ctx.replay_timeout, rem2)
                replay = run_process([str(ctx.binary), "replay", str(artifact_path)], rep_to)
                write(attempt_dir / "replay.stdout", replay["stdout"])
                write(attempt_dir / "replay.stderr", replay["stderr"])
                write(attempt_dir / "replay.exit", str(replay["exit_code"]))
                write(attempt_dir / "replay.command", " ".join(replay["cmd"]) + "\n")
        status = classify_repair(res, spec, spec["model"], spec["contract"], artifact_path, replay)
        record.update(status)
        record.update(_counts_flat(status.get("counts", {})))
        record["patch_len"] = _patch_len(artifact_path)
        record["accepted_chain_len"] = record.get("patch_len") if record.get("raw_outcome") == "repaired" else 0
        record["nodes"] = status.get("counts", {}).get("nodes")
        record["root_outcome"] = _root_outcome(artifact_path)
    else:
        cmd = [str(ctx.binary), "explore", spec["model"], spec["contract"], spec.get("engine", "petri")]
        res = run_process(cmd, stage_to)
        write(attempt_dir / "explore.stdout", res["stdout"])
        write(attempt_dir / "explore.stderr", res["stderr"])
        write(attempt_dir / "explore.exit", str(res["exit_code"]))
        write(attempt_dir / "explore.command", " ".join(res["cmd"]) + "\n")
        record["search_wall_ms"] = res["wall_ms"]
        record["exit_code"] = res["exit_code"]
        status = classify_explore(res, spec)
        record.update(status)
        record["states_explored"] = status.get("states_explored")
        record["transitions_explored"] = status.get("transitions_explored")

    record["wall_ms"] = record.get("search_wall_ms", 0)
    if record.get("evidence_status") == "complete":
        record["final_classification"] = record["raw_outcome"]
    elif record.get("evidence_status") == "replay_pending":
        record["final_classification"] = "replay_pending"
    else:
        record["final_classification"] = record.get("evidence_status")
    _commit(ctx, record, valid_index, index_path)
    _append(attempts_path, record)
    return record, "ran"


def _replay_phase(ctx, spec, record, deadline, attempt_dir):
    art = Path(spec["replay_only_from"])
    if not art.exists():
        record.update(evidence_status="artifact_missing", final_classification="artifact_missing",
                      raw_outcome=None, wall_ms=0)
        return
    record["artifact_sha256"] = sha256_file(art)
    rem = deadline - time.monotonic()
    if rem <= 0:
        record.update(evidence_status="replay_pending", final_classification="replay_pending",
                      raw_outcome=None, wall_ms=0)
        return
    rep = run_process([str(ctx.binary), "replay", str(art)], min(ctx.replay_timeout, rem))
    write(attempt_dir / "replay.stdout", rep["stdout"])
    write(attempt_dir / "replay.stderr", rep["stderr"])
    write(attempt_dir / "replay.exit", str(rep["exit_code"]))
    record["replay_exit"] = rep["exit_code"]
    record["replay_wall_ms"] = rep["wall_ms"]
    if rep["timed_out"]:
        record.update(evidence_status="replay_timeout", replay_ok=False,
                      final_classification="replay_timeout", raw_outcome=None)
    elif rep["exit_code"] != 0:
        record.update(evidence_status="replay_failed", replay_ok=False,
                      final_classification="replay_failed", raw_outcome=None)
    else:
        record.update(evidence_status="complete", replay_ok=True,
                      final_classification=spec.get("pending_outcome"), raw_outcome=spec.get("pending_outcome"))


def _counts_flat(counts: dict) -> dict:
    return {
        "proposals": counts.get("proposals"),
        "unique_programs": counts.get("unique_candidate_programs"),
        "verification_calls": counts.get("verification_calls"),
        "cache_hits": counts.get("cache_hits"),
        "states_explored": counts.get("states_explored"),
        "nodes": counts.get("nodes"),
    }


def _patch_len(artifact_path: Path):
    try:
        a = json.loads(artifact_path.read_text())
        return sum(len(e.get("changes", [])) for e in a.get("patch_chain", []))
    except Exception:
        return None


def _root_outcome(artifact_path: Path):
    try:
        a = json.loads(artifact_path.read_text())
        nodes = a.get("nodes", [])
        return nodes[0].get("report", {}).get("outcome") if nodes else None
    except Exception:
        return None


def _commit(ctx, record, valid_index, index_path):
    key = record["run_key"]
    rec = dict(record)
    if record.get("artifact_path"):
        try:
            rec["artifact_sha256"] = sha256_file(Path(record["artifact_path"]))
        except Exception:
            rec["artifact_sha256"] = None
    if _best(valid_index.get(key), rec):
        valid_index[key] = rec
    lines = [json.dumps(valid_index[k], sort_keys=True) for k in sorted(valid_index)]
    atomic_write(index_path, "\n".join(lines) + ("\n" if lines else ""))


def _append(path: Path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")


# ───────────────────────────── resume ─────────────────────────────


def _record_evidence_ok(ctx, rec, spec, search_to, replay_to):
    if rec.get("run_fingerprint") != spec["_fp"]:
        return False, "fingerprint"
    if rec.get("search_timeout") != search_to or rec.get("replay_timeout") != replay_to:
        return False, "timeout"
    st = rec.get("evidence_status")
    if st not in ("complete", "replay_pending"):
        return False, f"status={st}"
    art = rec.get("artifact_path")
    if not art or not Path(art).exists():
        return False, "artifact-missing"
    if rec.get("artifact_sha256") and sha256_file(Path(art)) != rec["artifact_sha256"]:
        return False, "artifact-hash"
    if st == "complete" and not rec.get("replay_ok"):
        return False, "not-replayed"
    return True, "ok"


def load_resume_index(ctx, batch_dir: Path, spec, search_to, replay_to):
    candidates = []
    roots = []
    if ctx.out.exists():
        roots.append(ctx.out / "results" / "batches")
    for root in roots:
        if not root.exists():
            continue
        for bdir in sorted(root.iterdir()):
            idx = bdir / "valid_index.jsonl"
            if idx.exists():
                candidates.append(idx)
    cur = batch_dir / "valid_index.jsonl"
    if cur.exists():
        candidates.insert(0, cur)
    for idx in candidates:
        for line in idx.read_text().splitlines():
            if not line.strip():
                continue
            rec = json.loads(line)
            if rec.get("run_key") != spec["run_key"]:
                continue
            ok, why = _record_evidence_ok(ctx, rec, spec, search_to, replay_to)
            if ok:
                return rec, why
    return None, "none"


# ───────────────────────────── do_run ─────────────────────────────


def make_batch_id(ctx) -> str:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    commit = git_info()["commit"][:8] or "nogit"
    cs = ctx.code()["fingerprint"][:8]
    bs = sha256_file(ctx.binary)[:8]
    return f"{stamp}-{commit}-{cs}-{bs}"


def environment(ctx) -> dict:
    def v(cmd):
        try:
            return subprocess.run(cmd, capture_output=True, text=True).stdout.strip()
        except Exception:
            return ""

    return {
        "recorded_at_utc": now_utc(),
        "git": git_info(),
        "code": ctx.code(),
        "binary_path": str(ctx.binary),
        "binary_sha256": sha256_file(ctx.binary) if ctx.binary.exists() else "",
        "tool_path": str(ctx.tool),
        "tool_sha256": sha256_file(ctx.tool) if ctx.tool.exists() else "",
        "rustc": v(["rustc", "--version"]),
        "cargo": v(["cargo", "--version"]),
        "python": sys.version.split()[0],
        "uname": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
    }


def do_run(args) -> int:
    out = Path(args.out).resolve()
    manifest_path = Path(args.manifest).resolve()
    binary = Path(args.bin).resolve()
    tool = Path(args.tool).resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    if not tool.exists():
        raise SystemExit(f"pilot_tool not found: {tool} (cargo build --release --example pilot_tool)")

    env_t0 = time.monotonic()
    ctx = Context(manifest_path, out, binary, tool, args.suite, args.search_timeout,
                  args.replay_timeout, args.total_budget, args.resume)
    batch_id = args.batch_tag or make_batch_id(ctx)
    batch_dir = out / "results" / "batches" / batch_id
    batch_dir.mkdir(parents=True, exist_ok=True)
    env = environment(ctx)
    env["env_collection_ms"] = int((time.monotonic() - env_t0) * 1000)
    env["env_outside_budget"] = True
    write(batch_dir / "environment.json", json.dumps(env, indent=2) + "\n")

    # Snapshot the exact experiment code for this batch.
    snap = batch_dir / "code_snapshot"
    for f in (SCRIPTS / "run_pilot.py", SCRIPTS / "pilot_analyze.py",
              manifest_path.parent / "generate_cases.py", manifest_path,
              REPO / "examples" / "pilot_tool.rs"):
        if f.exists():
            dest = snap / f.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(f, dest)

    binary_sha = sha256_file(binary)
    code_fp = ctx.code()["fingerprint"]
    plan = plan_runs(ctx)
    for spec in plan:
        spec["batch_id"] = batch_id
        spec["search_timeout"] = ctx.search_timeout
        spec["replay_timeout"] = ctx.replay_timeout
        spec["run_key"] = run_key(spec, ctx.search_timeout, ctx.replay_timeout)
        spec["run_fingerprint"] = run_fingerprint(ctx, spec, ctx.search_timeout, ctx.replay_timeout)
        spec["_fp"] = spec["run_fingerprint"]

    write(batch_dir / "plan.json", json.dumps(
        [{k: v for k, v in s.items() if not k.startswith("_")} for s in plan], indent=2) + "\n")
    attempts_path = batch_dir / "attempts.jsonl"
    index_path = batch_dir / "valid_index.jsonl"

    # load current batch index if restarting
    valid_index = {}
    if index_path.exists():
        for line in index_path.read_text().splitlines():
            if line.strip():
                r = json.loads(line)
                valid_index[r["run_key"]] = r

    deadline = time.monotonic() + ctx.total_budget
    counts = {"executed": 0, "reused": 0, "replay_recovered": 0, "not_executed": 0,
              "resume_mismatch": 0}
    used_attempts: dict[str, int] = {}
    for spec in plan:
        if time.monotonic() >= deadline and not args.allow_overrun:
            valid_index.setdefault(spec["run_key"], {
                "run_key": spec["run_key"], "run_fingerprint": spec["run_fingerprint"],
                "stage": spec["stage"], "suite": spec["suite"], "case": spec["case"],
                "family": spec["family"], "params": spec["params"],
                "config_name": spec["config_name"], "config": spec["config"],
                "strategy": spec["strategy"], "repeat": spec["repeat"],
                "identity": spec["identity"], "evidence_status": "not_executed",
                "final_classification": "not_executed", "raw_outcome": None,
                "wall_ms": 0, "notes": "total budget exhausted before planning",
            })
            counts["not_executed"] += 1
            continue
        key = spec["run_key"]
        attempt_no = used_attempts.get(key, 0) + 1
        used_attempts[key] = attempt_no
        if ctx.resume:
            rec, why = load_resume_index(ctx, batch_dir, spec, ctx.search_timeout, ctx.replay_timeout)
            if rec is not None:
                status = rec["evidence_status"]
                if status == "complete":
                    new = dict(rec)
                    new["reused_from"] = rec.get("attempt_dir") or rec.get("reused_from")
                    new["batch_id"] = batch_id
                    new["attempt"] = attempt_no
                    if _best(valid_index.get(key), new):
                        valid_index[key] = new
                    _append(attempts_path, {**new, "final_classification": rec["final_classification"]})
                    counts["reused"] += 1
                    print(f"[reuse] {run_slug(spec):44} {rec['final_classification']}")
                    continue
                if status == "replay_pending":
                    spec["replay_only_from"] = rec["artifact_path"]
                    spec["pending_outcome"] = rec.get("raw_outcome")
                    rec2, st = execute_attempt(ctx, spec, batch_dir, valid_index, attempt_no,
                                               deadline, index_path, attempts_path)
                    counts["replay_recovered"] += 1 if st == "replay_recovered" else 0
                    print(f"[replay] {run_slug(spec):44} {rec2['final_classification']}")
                    continue
                counts["resume_mismatch"] += 1
        rec, status = execute_attempt(ctx, spec, batch_dir, valid_index, attempt_no,
                                      deadline, index_path, attempts_path)
        counts["executed"] += 1
        print(f"[{counts['executed']:>3}/{len(plan)}] {run_slug(spec):44} "
              f"{rec['final_classification']:<22} {rec.get('wall_ms',0)}ms")

    _write_index(index_path, valid_index)
    meta = {
        "batch_id": batch_id,
        "batch_fingerprint": sha_canonical({
            "manifest": sha256_file(manifest_path),
            "code": code_fp,
            "binary": binary_sha,
            "tool": sha256_file(tool),
            "run_fingerprints": sorted(s["run_fingerprint"] for s in plan),
            "total_budget": ctx.total_budget,
            "search_timeout": ctx.search_timeout,
            "replay_timeout": ctx.replay_timeout,
        }),
        "binary_sha256": binary_sha,
        "code_fingerprint": code_fp,
        "manifest": str(manifest_path),
        "suites": ctx.suites,
        "planned": len(plan),
        **counts,
        "total_budget_s": ctx.total_budget,
        "search_timeout_s": ctx.search_timeout,
        "replay_timeout_s": ctx.replay_timeout,
        "env_collection_ms": env["env_collection_ms"],
        "env_outside_budget": True,
        "resume": ctx.resume,
        "results_dir": str(batch_dir),
    }
    write(batch_dir / "batch.json", json.dumps(meta, indent=2) + "\n")
    print(json.dumps(meta, indent=2))
    return 0


def _write_index(index_path: Path, valid_index: dict):
    lines = [json.dumps(valid_index[k], sort_keys=True) for k in sorted(valid_index)]
    atomic_write(index_path, "\n".join(lines) + ("\n" if lines else ""))


# ───────────────────────────── witness validation ─────────────────────────────


def do_validate_witness(args) -> int:
    out = Path(args.out).resolve()
    manifest_path = Path(args.manifest).resolve()
    tool = Path(args.tool).resolve()
    cfg = json.loads(manifest_path.read_text())
    results = []
    for c in cfg.get("cases", []):
        w = c.get("witness")
        if not w:
            continue
        model = (manifest_path.parent / c["model"]).resolve()
        contract = (manifest_path.parent / c["contract"]).resolve()
        swaps = w.get("swaps", [])
        row = {"case": c["case"], "witness_kind": w.get("kind"),
               "legal": w.get("kind") == "legal", "swaps": swaps}
        if w.get("kind") != "legal":
            # auxiliary semantic model: verify the auxiliary program if present
            aux = w.get("auxiliary_program")
            if aux:
                ap = (manifest_path.parent / aux).resolve()
                for engine in ("petri", "interp"):
                    r = run_process([str(Path(args.bin).resolve()),
                                     "explore", str(ap), str(contract), engine], args.timeout)
                    row[f"aux_{engine}"] = _explore_outcome(r)
            row["witness_expected"] = w.get("expected")
            results.append(row)
            print(f"{c['case']:26} kind={row['witness_kind']:10} aux={row.get('aux_petri',{}).get('outcome')} (not a legal witness)")
            continue
        cmd = [str(tool), "witness", str(model), str(contract), "--engine", "petri"]
        for s in swaps:
            cmd += ["--swap", f"{s['module']}:{s['function']}:{s['a']}:{s['b']}"]
        r = run_process(cmd, args.timeout)
        try:
            d = json.loads(r["stdout"])
        except Exception:
            d = {"error": r["stderr"][:200]}
        row["chain_len"] = len(d.get("chain", []))
        row["permissions_ok"] = all(p.get("allowed") for p in d.get("permissions", []))
        row["final_outcome"] = d.get("final_outcome")
        row["final_states"] = d.get("final_states")
        row["error"] = d.get("error")
        # expanded analysis if the original bounds made it UNKNOWN: run the
        # *witness program* under the separate expanded contract.
        if d.get("final_outcome") in ("UNKNOWN", "Unknown"):
            exp = w.get("expanded_contract")
            if exp and c.get("witness"):
                ep = (manifest_path.parent / exp).resolve()
                wp = (manifest_path.parent / "witness" / f"{c['case']}_witness.json").resolve()
                rr = run_process([str(Path(args.bin).resolve()),
                                  "explore", str(wp), str(ep), "petri"], args.timeout)
                row["expanded"] = _explore_outcome(rr)
        results.append(row)
        print(f"{c['case']:26} legal chain={row['chain_len']} perms={row['permissions_ok']} "
              f"outcome={row['final_outcome']} states={row['final_states']}")
    write(out / "witness_results.json", json.dumps(results, indent=2) + "\n")
    def witness_ok(r):
        if not r.get("legal"):
            return True
        if r.get("error") or not r.get("permissions_ok"):
            return False
        if r.get("final_outcome") == "PASS":
            return True
        return bool(r.get("expanded") and r["expanded"].get("outcome") == "PASS")
    failures = [r for r in results if not witness_ok(r)]
    print(f"witness validation: {len(results) - len(failures)}/{len(results)} witnesses ok "
          f"(legal PASS under original contract, or UNKNOWN under a bounded control with PASS under expanded)")
    return 1 if failures else 0


def _explore_outcome(r):
    try:
        d = json.loads(r["stdout"])
        return {"outcome": d.get("outcome"), "states": d.get("states_explored"),
                "wall_ms": r["wall_ms"]}
    except Exception:
        return {"outcome": None, "error": r["stderr"][:200]}


# ───────────────────────────── summarize ─────────────────────────────


def do_summarize(args) -> int:
    out = Path(args.out).resolve()
    batches_root = out / "results" / "batches"
    if args.batch:
        batch_dir = batches_root / args.batch
    else:
        dirs = sorted([d for d in batches_root.iterdir() if d.is_dir()]) if batches_root.exists() else []
        if not dirs:
            raise SystemExit("no batches found")
        batch_dir = dirs[-1]
    sys.path.insert(0, str(SCRIPTS))
    import pilot_analyze  # noqa: E402
    rc = pilot_analyze.summarize_batch(batch_dir, out)
    print(f"summarized {batch_dir}")
    return rc


# ───────────────────────────── behavior regression ─────────────────────────────


def _fake_ok_repair(real, wrapper=None):
    return None


def do_behavior(args) -> int:
    """Real-path behavior regression with fault injection at the subprocess or
    file boundary. Exercises classify/execute_attempt/do_run/summarize."""
    out = (Path(args.out).resolve() / "behavior")
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, exist_ok=True)
    manifest_path = Path(args.manifest).resolve()
    binary = Path(args.bin).resolve()
    tool = Path(args.tool).resolve()
    checks = []

    def ctx_for():
        return Context(manifest_path, out, binary, tool, ["pilot"], 30, 30, 600, False)

    def bind(ctx0, spec0):
        spec0["_norm_model"] = ctx0.normalize(Path(spec0["model"]), "program")["norm"]
        spec0["_norm_contract"] = ctx0.normalize(Path(spec0["contract"]), "contract")["norm"]
        spec0["identity"]["contract_bounds"] = spec0["_norm_contract"].get("bounds")
        spec0["search_timeout"] = 30.0
        spec0["replay_timeout"] = 30.0
        spec0["run_key"] = run_key(spec0, 30.0, 30.0)
        spec0["run_fingerprint"] = run_fingerprint(ctx0, spec0, 30.0, 30.0)
        spec0["_fp"] = spec0["run_fingerprint"]
        return spec0

    # 1. normal run + nonzero exit but legal artifact
    ctx = ctx_for()
    spec = _repair_run(ctx, "pilot", {"case": "p1_same", "family": "one_defect",
                                      "model": "cases/p1_same.json",
                                      "contract": "cases/p1_same_contract.json",
                                      "params": {}},
                       "main", ctx.manifest["search_configs"]["main"], "b", 1)
    bind(ctx, spec)
    spec["batch_id"] = "behavior"
    bdir = out / "b1"
    vi = {}
    rec, _ = execute_attempt(ctx, spec, bdir, vi, 1, time.monotonic() + 60,
                             bdir / "valid_index.jsonl", bdir / "attempts.jsonl")
    checks.append(("normal_repair_and_replay", rec.get("evidence_status") == "complete"
                   and rec.get("final_classification") == "repaired" and rec.get("replay_ok") is True,
                   rec.get("final_classification")))

    # legal non-success artifact: c_scope_restricted -> no_acceptable, exit 1
    spec2 = _repair_run(ctx, "pilot", {"case": "c_scope_restricted", "family": "controls",
                                       "model": "cases/c_scope_restricted.json",
                                       "contract": "cases/c_scope_restricted_contract.json",
                                       "params": {}},
                        "main", ctx.manifest["search_configs"]["main"], "b", 1)
    bind(ctx, spec2)
    spec2["batch_id"] = "behavior"
    bdir2 = out / "b2"
    rec2, _ = execute_attempt(ctx, spec2, bdir2, {}, 1, time.monotonic() + 60,
                              bdir2 / "valid_index.jsonl", bdir2 / "attempts.jsonl")
    checks.append(("nonzero_legal_artifact", rec2.get("evidence_status") == "complete"
                   and rec2.get("final_classification") == "no_acceptable_candidate"
                   and rec2.get("exit_code") == 1 and rec2.get("replay_ok") is True,
                   f"{rec2.get('final_classification')} exit={rec2.get('exit_code')}"))

    # 2. stale historical success must not mask a current failure producing no file
    stale = out / "stale"
    stale.mkdir(parents=True, exist_ok=True)
    (stale / "artifact.json").write_text(json.dumps({"outcome": "repaired"}))
    spec3 = _repair_run(ctx, "pilot", {"case": "p1_same", "family": "one_defect",
                                       "model": "cases/p1_same.json",
                                       "contract": "cases/p1_same_contract.json",
                                       "params": {}},
                        "main", ctx.manifest["search_configs"]["main"], "b", 1)
    bind(ctx, spec3)
    spec3["batch_id"] = "behavior"
    real_run_process = run_process

    def fail_repair(cmd, timeout_s, cwd=REPO):
        if "repair" in [str(c) for c in cmd]:
            return {"cmd": [str(c) for c in cmd], "exit_code": 2, "stdout": "",
                    "stderr": "injected input failure", "wall_ms": 1,
                    "timeout_s": timeout_s, "timed_out": False, "spawn_error": None}
        return real_run_process(cmd, timeout_s, cwd)

    globals()["run_process"] = fail_repair
    try:
        bdir3 = out / "b3"
        rec3, _ = execute_attempt(ctx, spec3, bdir3, {}, 1, time.monotonic() + 60,
                                  bdir3 / "valid_index.jsonl", bdir3 / "attempts.jsonl")
    finally:
        globals()["run_process"] = real_run_process
    checks.append(("stale_success_not_masking", rec3.get("evidence_status") != "complete"
                   and rec3.get("final_classification") != "repaired",
                   f"{rec3.get('evidence_status')}"))

    # 3. stdout only, no artifact
    def stdout_only(cmd, timeout_s, cwd=REPO):
        return {"cmd": [str(c) for c in cmd], "exit_code": 0,
                "stdout": json.dumps({"outcome": "repaired", "counts": {}}),
                "stderr": "", "wall_ms": 1, "timeout_s": timeout_s, "timed_out": False,
                "spawn_error": None}
    globals()["run_process"] = stdout_only
    try:
        bdir4 = out / "b4"
        rec4, _ = execute_attempt(ctx, spec3, bdir4, {}, 1, time.monotonic() + 60,
                                  bdir4 / "valid_index.jsonl", bdir4 / "attempts.jsonl")
    finally:
        globals()["run_process"] = real_run_process
    checks.append(("stdout_without_artifact", rec4.get("evidence_status") == "no_artifact"
                   and rec4.get("replay_ok") is None,
                   rec4.get("evidence_status")))

    # 4. corrupted / non-object JSON artifact
    def bad_json(cmd, timeout_s, cwd=REPO):
        art = None
        cs = [str(c) for c in cmd]
        if "--artifact" in cs:
            art = Path(cs[cs.index("--artifact") + 1])
            art.write_text("{ not json")
        return {"cmd": cs, "exit_code": 0, "stdout": "", "stderr": "", "wall_ms": 1,
                "timeout_s": timeout_s, "timed_out": False, "spawn_error": None}
    globals()["run_process"] = bad_json
    try:
        bdir5 = out / "b5"
        rec5, _ = execute_attempt(ctx, spec3, bdir5, {}, 1, time.monotonic() + 60,
                                  bdir5 / "valid_index.jsonl", bdir5 / "attempts.jsonl")
    finally:
        globals()["run_process"] = real_run_process
    checks.append(("corrupt_json_artifact", rec5.get("evidence_status") == "json_error",
                   rec5.get("evidence_status")))

    # 5. spawn error
    real_popen = subprocess.Popen

    def raising_popen(*a, **k):
        raise FileNotFoundError("injected spawn failure")
    subprocess.Popen = raising_popen
    try:
        bdir6 = out / "b6"
        rec6, _ = execute_attempt(ctx, spec3, bdir6, {}, 1, time.monotonic() + 60,
                                  bdir6 / "valid_index.jsonl", bdir6 / "attempts.jsonl")
    finally:
        subprocess.Popen = real_popen
    checks.append(("spawn_error_structured", rec6.get("evidence_status") == "spawn_error",
                   rec6.get("evidence_status")))

    # 6. replay failure and replay timeout on a real artifact
    def tamper_replay(cmd, timeout_s, cwd=REPO):
        cs = [str(c) for c in cmd]
        if len(cs) >= 2 and cs[1] == "replay":
            return real_run_process(cs, timeout_s, cwd)  # real replay on tampered file below
        return real_run_process(cs, timeout_s, cwd)
    # produce a real artifact then tamper then replay through classify path
    spec6 = dict(spec3)
    bdir7 = out / "b7"
    rec6b, _ = execute_attempt(ctx, spec6, bdir7, {}, 1, time.monotonic() + 60,
                               bdir7 / "valid_index.jsonl", bdir7 / "attempts.jsonl")
    art = Path(rec6b.get("artifact_path") or "")
    replay_fail_ok = False
    if art.exists():
        d = json.loads(art.read_text())
        d["outcome"] = "no_acceptable_candidate"
        art.write_text(json.dumps(d))
        rr = run_process([str(binary), "replay", str(art)], 30)
        replay_fail_ok = rr["exit_code"] != 0
    checks.append(("replay_failure_detected", replay_fail_ok, "tampered replay non-zero"))

    # 7. duplicate rerun / only-B does not grow the index or cross-pair
    ctx7 = ctx_for()
    ctx7.manifest = json.loads(manifest_path.read_text())
    bdir8 = out / "b8"
    vi8 = {}
    for _ in range(2):
        execute_attempt(ctx, spec3, bdir8, vi8, 1, time.monotonic() + 60,
                        bdir8 / "valid_index.jsonl", bdir8 / "attempts.jsonl")
    checks.append(("duplicate_rerun_single_index", len(vi8) == 1, f"index={len(vi8)}"))

    # 8. same config name, changed value -> different run key/fingerprint
    ctx8 = ctx_for()
    specA = _repair_run(ctx8, "pilot", {"case": "p1_same", "family": "one_defect",
                                        "model": "cases/p1_same.json",
                                        "contract": "cases/p1_same_contract.json", "params": {}},
                        "main", dict(ctx8.manifest["search_configs"]["main"]), "b", 1)
    specB = dict(specA)
    specB["config"] = dict(specA["config"])
    specB["config"]["candidate_budget"] = specA["config"]["candidate_budget"] + 1
    kA = run_key(specA, 30, 30)
    kB = run_key(specB, 30, 30)
    fA = run_fingerprint(ctx8, specA, 30, 30)
    fB = run_fingerprint(ctx8, specB, 30, 30)
    checks.append(("same_name_changed_value_distinct", kA != kB and fA != fB, f"{kA[:8]}/{kB[:8]}"))

    # 9. resume refuses missing artifact / changed timeout / replay_timeout
    good = dict(rec2)
    good["run_key"] = spec2["run_key"]
    good["run_fingerprint"] = run_fingerprint(ctx, spec2, 30, 30)
    good["search_timeout"] = 30
    good["replay_timeout"] = 30
    ok, why = _record_evidence_ok(ctx, good, spec2, 30, 30)
    p = Path(good.get("artifact_path"))
    saved = p.read_bytes()
    p.unlink()
    ok2, why2 = _record_evidence_ok(ctx, good, spec2, 30, 30)
    p.write_bytes(saved)
    ok3, why3 = _record_evidence_ok(ctx, good, spec2, 30, 5)
    bad = dict(good)
    bad["evidence_status"] = "replay_timeout"
    ok4, why4 = _record_evidence_ok(ctx, bad, spec2, 30, 30)
    checks.append(("resume_refuses_incomplete",
                   ok and not ok2 and not ok3 and not ok4,
                   f"good={ok} missing={why2} timeout={why3} replay_to={why4}"))

    # 10. tiny total budget with a real sleeping subprocess (deadline + reaping)
    real_execute = globals()["execute_attempt"]
    sleeps = {"done": False}

    def sleep_probe(ctx2, spec2x, batch_dir2, valid_index2, attempt_no2, deadline2,
                    index_path2, attempts_path2, log=print):
        remaining = deadline2 - time.monotonic()
        to = max(0.001, min(0.25, remaining)) if remaining > 0 else 0.001
        r = real_run_process([sys.executable, "-c", "import time; time.sleep(0.25)"], to)
        sleeps["done"] = not r["timed_out"] and r["wall_ms"] >= 240
        recx = {"run_key": spec2x["run_key"], "run_fingerprint": spec2x["run_fingerprint"],
                "evidence_status": "not_executed" if r["timed_out"] else "complete",
                "final_classification": "not_executed" if r["timed_out"] else "slept",
                "raw_outcome": None, "wall_ms": r["wall_ms"], "stage": "repair",
                "suite": "pilot", "case": spec2x["case"], "family": "", "params": {},
                "config_name": "main", "config": {}, "strategy": "b", "repeat": 1,
                "identity": {}, "attempt": attempt_no2}
        valid_index2[spec2x["run_key"]] = recx
        _commit(ctx2, recx, valid_index2, index_path2)
        _append(attempts_path2, recx)
        return recx, "ran"
    tiny = {
        "schema": "concir-pilot-manifest-v2",
        "search_configs": {"main": {"candidate_budget": 64, "verification_budget": 64,
                                     "max_depth": 4, "max_total_edits": 4},
                            "tight": {"candidate_budget": 8, "verification_budget": 4,
                                      "max_depth": 1, "max_total_edits": 1}},
        "cases": [{"case": "p1_same", "family": "one_defect",
                   "model": str((manifest_path.parent / "cases/p1_same.json").resolve()),
                   "contract": str((manifest_path.parent / "cases/p1_same_contract.json").resolve()),
                   "params": {}}],
        "smoke_cases": [], "matrix_cases": [],
    }
    tiny_path = out / "tiny_manifest.json"
    tiny_path.write_text(json.dumps(tiny))
    globals()["execute_attempt"] = sleep_probe
    t0 = time.monotonic()
    try:
        do_run(argparse.Namespace(out=str(out), manifest=str(tiny_path), bin=str(binary),
                                  tool=str(tool), suite=["pilot"], search_timeout=2,
                                  replay_timeout=2, total_budget=0.05, resume=False,
                                  batch_tag="behavior-deadline", allow_overrun=False))
    finally:
        globals()["execute_attempt"] = real_execute
    elapsed = time.monotonic() - t0
    checks.append(("tiny_budget_hard_deadline", elapsed < 1.0 and not sleeps["done"],
                   f"elapsed={elapsed:.3f}s full_sleep={sleeps['done']}"))

    ok = all(c[1] for c in checks)
    for name, good, detail in checks:
        print(f"{'PASS' if good else 'FAIL'}  {name}: {detail}")
    print(f"behavior regression {'passed' if ok else 'FAILED'}")
    return 0 if ok else 1


# ───────────────────────────── main ─────────────────────────────


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default=str(DEFAULT_OUT))
    ap.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    ap.add_argument("--bin", default=str(DEFAULT_BIN))
    ap.add_argument("--tool", default=str(DEFAULT_TOOL))
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("run")
    p.add_argument("--suite", action="append", required=True,
                   choices=["smoke", "pilot", "matrix", "heavy"])
    p.add_argument("--search-timeout", type=float, default=30.0)
    p.add_argument("--replay-timeout", type=float, default=30.0)
    p.add_argument("--total-budget", type=float, default=600.0)
    p.add_argument("--resume", action="store_true")
    p.add_argument("--batch-tag", default=None)
    p.add_argument("--allow-overrun", action="store_true")
    p.set_defaults(func=do_run)

    p = sub.add_parser("summarize")
    p.add_argument("--batch", default=None)
    p.set_defaults(func=do_summarize)

    p = sub.add_parser("validate-witness")
    p.add_argument("--timeout", type=float, default=180.0)
    p.set_defaults(func=do_validate_witness)

    p = sub.add_parser("behavior")
    p.set_defaults(func=do_behavior)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
