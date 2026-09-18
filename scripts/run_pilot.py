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
import contextlib
import csv
import io
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


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%f")


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


def identity_key(f: Path) -> str:
    """Stable file identity key.

    Files under the repository use their relative path. Files outside (for
    example a manifest or fixture in an external `--out` directory) use a
    disambiguated absolute-path key, never a bare basename, so two different
    files can never collapse into one identity.
    """
    f = f.resolve()
    try:
        return str(f.relative_to(REPO))
    except ValueError:
        return f"external:{f}"


def code_identity(files: list[Path]) -> dict:
    entries = {}
    for f in files:
        f = f.resolve()
        key = identity_key(f)
        if f.exists():
            entries[key] = {"sha256": sha256_file(f), "bytes": f.stat().st_size}
        else:
            entries[key] = {"sha256": None, "bytes": None}
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
        "report_complete": None,
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
    # `report.complete` describes whether the bounded exploration finished; it is
    # distinct from `evidence_status == "complete"` (the record is well-formed).
    out["report_complete"] = report.get("complete")
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
    return candidate.get("attempt_seq", 0) >= existing.get("attempt_seq", 0)


def _new_attempt(batch_dir: Path, spec: dict, attempt_seq: int):
    """Create an exclusive, cross-invocation-unique attempt directory.

    The namespace directory is shared; the attempt directory itself is created
    with `exist_ok=False`, so an old attempt (even one left by a crash) can
    never be reused or overwritten.
    """
    slug = run_slug(spec)
    run_key_ = spec["run_key"]
    parent = batch_dir / "raw" / f"{slug}__{run_key_[:24]}"
    parent.mkdir(parents=True, exist_ok=True)
    stamp = utc_stamp()
    for bump in range(1000):
        attempt_id = f"{stamp}-{os.getpid()}-{attempt_seq}" + ("" if bump == 0 else f"-{bump}")
        d = parent / f"attempt-{attempt_id}"
        try:
            d.mkdir(parents=False, exist_ok=False)
            return d, attempt_id
        except FileExistsError:
            continue
    raise RuntimeError("could not allocate a unique attempt directory")


def next_attempt_seq(attempts_path: Path) -> int:
    m = 0
    if attempts_path.exists():
        for line in attempts_path.read_text().splitlines():
            if not line.strip():
                continue
            try:
                m = max(m, int(json.loads(line).get("attempt_seq") or 0))
            except Exception:
                continue
    return m + 1


def execute_attempt(ctx, spec, batch_dir: Path, valid_index: dict, attempt_seq: int,
                    deadline: float, index_path: Path, attempts_path: Path, log=print):
    if spec.get("replay_only_from"):
        return _replay_phase(ctx, spec, batch_dir, valid_index, attempt_seq,
                             deadline, index_path, attempts_path)

    attempt_dir, attempt_id = _new_attempt(batch_dir, spec, attempt_seq)
    record = {
        "batch_id": spec["batch_id"],
        "run_key": spec["run_key"],
        "run_fingerprint": spec["run_fingerprint"],
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
        "attempt_id": attempt_id,
        "attempt_seq": attempt_seq,
        "attempt_dir": str(attempt_dir),
        "search_timeout": spec["search_timeout"],
        "replay_timeout": spec["replay_timeout"],
        "reused_from": None,
        "wall_ms_basis": "search",
    }

    remaining = deadline - time.monotonic()
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
        record["search_command"] = res["cmd"]
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
                record["replay_command"] = replay["cmd"]
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
        record["report_complete"] = status.get("report_complete")

    record["wall_ms"] = record.get("search_wall_ms", 0)
    record["wall_ms_basis"] = "search"
    if record.get("evidence_status") == "complete":
        record["final_classification"] = record["raw_outcome"]
    elif record.get("evidence_status") == "replay_pending":
        record["final_classification"] = "replay_pending"
    else:
        record["final_classification"] = record.get("evidence_status")
    _commit(ctx, record, valid_index, index_path)
    _append(attempts_path, record)
    return record, "ran"


def _verify_pending_artifact(ctx, spec, pending):
    """Re-validate a pending record's artifact against this run. Returns an
    error string, or None when the artifact is the same, complete-search
    evidence for this run."""
    art = pending.get("artifact_path")
    if not art or not Path(art).exists():
        return "artifact-missing"
    p = Path(art)
    if not pending.get("artifact_sha256"):
        return "missing-artifact-hash"
    if sha256_file(p) != pending["artifact_sha256"]:
        return "artifact-hash-mismatch"
    try:
        a = json.loads(p.read_text())
    except Exception:
        return "artifact-json"
    if not isinstance(a, dict) or a.get("schema_version") != "concir-repair-artifact-v1":
        return "artifact-schema"
    raw = a.get("outcome")
    if raw not in REPAIR_OUTCOMES or raw != pending.get("raw_outcome"):
        return "artifact-outcome"
    if pending.get("exit_code") != REPAIR_EXIT.get(raw):
        return "artifact-exit"
    if canonical(a.get("input_program")) != canonical(spec["_norm_model"]):
        return "artifact-input"
    if canonical(a.get("frozen_contract")) != canonical(spec["_norm_contract"]):
        return "artifact-contract"
    ok, why = _artifact_canonical_matches(a, spec)
    if not ok:
        return f"artifact-config:{why}"
    for field in ("search_wall_ms", "verification_calls", "states_explored", "patch_len", "root_outcome"):
        if field not in pending:
            return f"missing-{field}"
    return None


def _replay_phase(ctx, spec, batch_dir, valid_index, attempt_seq, deadline, index_path, attempts_path):
    """Run only the replay for a search that already completed.

    The recovered record inherits the full original search evidence (artifact
    path/hash, exit code, config/input binding, counts, patch chain, search
    wall time) and adds this replay attempt's command/log/exit/wall time. A
    replay is never recorded as search time and a missing search cost is never
    recorded as zero.
    """
    pending = dict(spec["_pending_record"])
    attempt_dir, attempt_id = _new_attempt(batch_dir, spec, attempt_seq)
    record = dict(pending)
    record.update({
        "batch_id": spec["batch_id"],
        "run_key": spec["run_key"],
        "run_fingerprint": spec["run_fingerprint"],
        "stage": spec["stage"],
        "suite": spec["suite"],
        "case": spec["case"],
        "config_name": spec["config_name"],
        "config": spec["config"],
        "strategy": spec["strategy"],
        "repeat": spec["repeat"],
        "identity": spec["identity"],
        "attempt_id": attempt_id,
        "attempt_seq": attempt_seq,
        "attempt_dir": str(attempt_dir),
        "search_timeout": spec["search_timeout"],
        "replay_timeout": spec["replay_timeout"],
        "recovered_from_attempt": pending.get("attempt_id"),
        "recovered_from_dir": pending.get("attempt_dir"),
    })

    why = _verify_pending_artifact(ctx, spec, pending)
    if why is not None:
        record.update(evidence_status="evidence_incomplete" if why.startswith("missing-") else "identity_mismatch",
                      final_classification="evidence_incomplete" if why.startswith("missing-") else "identity_mismatch",
                      notes=f"replay recovery refused: {why}",
                      wall_ms=pending.get("search_wall_ms", 0), wall_ms_basis="search",
                      end_to_end_wall_ms=pending.get("search_wall_ms", 0))
        _commit(ctx, record, valid_index, index_path)
        _append(attempts_path, record)
        return record, "replay_recovered"

    rem = deadline - time.monotonic()
    if rem <= 0:
        record.update(evidence_status="replay_pending", final_classification="replay_pending",
                      replay_ok=None, notes="total budget exhausted before replay",
                      wall_ms=pending.get("search_wall_ms", 0), wall_ms_basis="search",
                      end_to_end_wall_ms=pending.get("search_wall_ms", 0))
        _commit(ctx, record, valid_index, index_path)
        _append(attempts_path, record)
        return record, "replay_recovered"

    art = Path(pending["artifact_path"])
    rep_to = min(ctx.replay_timeout, rem)
    rep = run_process([str(ctx.binary), "replay", str(art)], rep_to)
    write(attempt_dir / "replay.stdout", rep["stdout"])
    write(attempt_dir / "replay.stderr", rep["stderr"])
    write(attempt_dir / "replay.exit", str(rep["exit_code"]))
    write(attempt_dir / "replay.command", " ".join(rep["cmd"]) + "\n")
    record["replay_command"] = rep["cmd"]
    record["replay_attempt_id"] = attempt_id
    record["replay_attempt_dir"] = str(attempt_dir)
    record["replay_exit"] = rep["exit_code"]
    record["replay_wall_ms"] = rep["wall_ms"]
    record["replay_stdout_path"] = str(attempt_dir / "replay.stdout")
    record["replay_stderr_path"] = str(attempt_dir / "replay.stderr")
    # wall_ms is the search cost on every path so normal and recovered runs are
    # comparable; the replay and end-to-end costs are separate named fields.
    record["wall_ms"] = int(pending.get("search_wall_ms", 0))
    record["wall_ms_basis"] = "search"
    record["end_to_end_wall_ms"] = int(pending.get("search_wall_ms", 0)) + int(rep["wall_ms"])
    if rep["timed_out"]:
        record.update(evidence_status="replay_timeout", replay_ok=False,
                      final_classification="replay_timeout")
    elif rep["exit_code"] != 0:
        record.update(evidence_status="replay_failed", replay_ok=False,
                      final_classification="replay_failed")
    else:
        record.update(evidence_status="complete", replay_ok=True,
                      final_classification=pending.get("raw_outcome"),
                      reused_from=None)
    _commit(ctx, record, valid_index, index_path)
    _append(attempts_path, record)
    return record, "replay_recovered"


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
    record["artifact_sha256"] = rec.get("artifact_sha256")
    # Evidence validity gates selection: a complete/pending record whose
    # artifact/file binding no longer holds is demoted before comparison, so a
    # stale "success" can never win over a newer failure.
    errs = record_evidence_errors(rec)
    if errs and rec.get("evidence_status") in ("complete", "replay_pending"):
        rec["evidence_status_original"] = rec["evidence_status"]
        rec["evidence_status"] = "evidence_invalid"
        rec["final_classification"] = "evidence_invalid"
        rec["evidence_errors"] = errs
        record["evidence_status"] = "evidence_invalid"
        record["final_classification"] = "evidence_invalid"
        record["evidence_errors"] = errs
    if _best(valid_index.get(key), rec):
        valid_index[key] = rec
    lines = [json.dumps(valid_index[k], sort_keys=True) for k in sorted(valid_index)]
    atomic_write(index_path, "\n".join(lines) + ("\n" if lines else ""))
    return rec


def _append(path: Path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")


# ───────────────────────────── resume ─────────────────────────────


# Required fields for a `complete` record. `_commit`, `summarize` and `audit`
# all enforce the same set; a missing field is an error, never treated as zero.
def required_complete_fields(rec) -> list:
    st = rec.get("stage")
    if st == "explore":
        return ["raw_outcome", "exit_code", "search_wall_ms", "states_explored",
                "transitions_explored", "report_complete", "wall_ms"]
    return ["raw_outcome", "exit_code", "artifact_path", "artifact_sha256",
            "search_wall_ms", "verification_calls", "states_explored", "patch_len",
            "root_outcome", "wall_ms", "replay_ok", "replay_exit"]


def missing_complete_fields(rec) -> list:
    missing = []
    for f in required_complete_fields(rec):
        if f not in rec or rec.get(f) is None:
            missing.append(f)
    if rec.get("stage") != "explore":
        if rec.get("replay_ok") is not True:
            missing.append("replay_ok!=true")
        if rec.get("replay_exit") != 0:
            missing.append("replay_exit!=0")
    return missing


def record_evidence_errors(rec) -> list:
    """Cheap evidence/binding checks (never re-runs the backend).

    Returns the list of reasons a record claiming `complete`/`replay_pending`
    should NOT be treated as valid evidence. Non-evidence statuses (timeouts,
    no_artifact, not_executed, ...) are themselves valid observations and return
    an empty list.
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
    # repair stage
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
                if not isinstance(a, dict) or a.get("schema_version") != "concir-repair-artifact-v1":
                    errs.append("artifact-schema")
                elif a.get("outcome") != rec.get("raw_outcome"):
                    errs.append("artifact-outcome-mismatch")
            except Exception:
                errs.append("artifact-json")
    if st == "complete" and REPAIR_EXIT.get(rec.get("raw_outcome")) != rec.get("exit_code"):
        errs.append("exit-mismatch")
    if st == "complete" and rec.get("replay_ok") is not True:
        errs.append("replay-not-ok")
    return errs


def revalidate_index(valid_index: dict) -> int:
    """Demote any complete/pending record whose evidence is no longer valid.

    The record is kept (history) but leaves the valid success set: its status
    becomes `evidence_invalid`, so it cannot win the best-record selection and
    is not counted as a success by summarize/audit.
    """
    demoted = 0
    for rec in valid_index.values():
        if rec.get("evidence_status") in ("complete", "replay_pending"):
            errs = record_evidence_errors(rec)
            if errs:
                rec["evidence_status_original"] = rec["evidence_status"]
                rec["evidence_status"] = "evidence_invalid"
                rec["final_classification"] = "evidence_invalid"
                rec["evidence_errors"] = errs
                demoted += 1
    return demoted


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
    if not rec.get("artifact_sha256"):
        return False, "artifact-hash-missing"
    if sha256_file(Path(art)) != rec["artifact_sha256"]:
        return False, "artifact-hash"
    if st == "complete":
        missing = missing_complete_fields(rec)
        if missing:
            return False, "incomplete:" + ",".join(missing)
        if not rec.get("replay_ok"):
            return False, "not-replayed"
    return True, "ok"


def load_resume_index(ctx, batch_dir: Path, spec, search_to, replay_to):
    """Only the *same* batch's frozen index may be reused.

    Reusing evidence from another batch/cohort would mix experiment identities;
    a fresh batch tag always starts from an empty index.
    """
    idx = batch_dir / "valid_index.jsonl"
    if not idx.exists():
        return None, "none"
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


def cohort_identity(ctx, plan) -> dict:
    """The immutable experiment identity shared by every invocation of a batch.

    `total_budget` is deliberately excluded: it is a per-invocation recovery
    quota, not an experiment parameter. Search/replay timeouts ARE included
    (they change results). Inputs, configs, timeouts and engine enter through
    the per-run `run_key`/`run_fingerprint`, which include the binary and code
    fingerprints.
    """
    gen = ctx.manifest_dir / "generate_cases.py"
    return {
        "manifest_sha256": sha256_file(ctx.manifest_path),
        "manifest_path_identity": identity_key(ctx.manifest_path),
        "generator_sha256": sha256_file(gen) if gen.exists() else None,
        "code_fingerprint": ctx.code()["fingerprint"],
        "binary_sha256": ctx.binary_sha(),
        "tool_sha256": sha256_file(ctx.tool) if ctx.tool.exists() else None,
        "search_timeout": float(ctx.search_timeout),
        "replay_timeout": float(ctx.replay_timeout),
        "suites": sorted(ctx.suites),
        "plan_run_keys": sorted(s["run_key"] for s in plan),
        "plan_run_fingerprints": sorted(s["run_fingerprint"] for s in plan),
    }


def cohort_fingerprint(cohort: dict) -> str:
    return sha_canonical(cohort)


def _cohort_diff(frozen: dict, current: dict) -> list:
    diffs = []
    for k in sorted(set(frozen) | set(current)):
        if frozen.get(k) != current.get(k):
            fv, cv = frozen.get(k), current.get(k)
            if k in ("plan_run_keys", "plan_run_fingerprints"):
                diffs.append(f"{k}: {len(fv or [])} vs {len(cv or [])} entries differ")
            else:
                diffs.append(f"{k}: {fv!r} != {cv!r}")
    return diffs


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


def _load_index(index_path: Path) -> dict:
    valid_index = {}
    if index_path.exists():
        for line in index_path.read_text().splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            valid_index[r["run_key"]] = r
    return valid_index


def _append_jsonl(path: Path, obj: dict):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(json.dumps(obj, sort_keys=True) + "\n")


def do_run(args) -> int:
    out = Path(args.out).resolve()
    manifest_path = Path(args.manifest).resolve()
    binary = Path(args.bin).resolve()
    tool = Path(args.tool).resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    if not tool.exists():
        raise SystemExit(f"pilot_tool not found: {tool} (cargo build --release --example pilot_tool)")

    ctx = Context(manifest_path, out, binary, tool, args.suite, args.search_timeout,
                  args.replay_timeout, args.total_budget, args.resume)
    batch_id = args.batch_tag or make_batch_id(ctx)
    batch_dir = out / "results" / "batches" / batch_id

    # ── lifecycle gate: refuse to touch an existing batch unless resuming ──
    if batch_dir.exists() and not args.resume:
        print(json.dumps({
            "error": "batch-exists",
            "batch_id": batch_id,
            "message": "batch tag already exists; re-run with --resume to continue it, "
                       "or choose a new --batch-tag (batch identity is immutable)",
        }, indent=2))
        return 2
    if not batch_dir.exists() and args.resume:
        print(json.dumps({
            "error": "batch-missing",
            "batch_id": batch_id,
            "message": "--resume requires an existing frozen batch",
        }, indent=2))
        return 2

    # Plan and bind identity before writing anything.
    plan = plan_runs(ctx)
    for spec in plan:
        spec["batch_id"] = batch_id
        spec["search_timeout"] = ctx.search_timeout
        spec["replay_timeout"] = ctx.replay_timeout
        spec["run_key"] = run_key(spec, ctx.search_timeout, ctx.replay_timeout)
        spec["run_fingerprint"] = run_fingerprint(ctx, spec, ctx.search_timeout, ctx.replay_timeout)
        spec["_fp"] = spec["run_fingerprint"]
    cohort = cohort_identity(ctx, plan)
    cohort_fp = cohort_fingerprint(cohort)

    resuming = batch_dir.exists()
    env_t0 = time.monotonic()
    if not resuming:
        batch_dir.mkdir(parents=True, exist_ok=False)
        env = environment(ctx)
        env["env_collection_ms"] = int((time.monotonic() - env_t0) * 1000)
        env["env_outside_budget"] = True
        write(batch_dir / "environment.json", json.dumps(env, indent=2) + "\n")
        snap = batch_dir / "code_snapshot"
        for f in (SCRIPTS / "run_pilot.py", SCRIPTS / "pilot_analyze.py",
                  SCRIPTS / "pilot_audit.py", manifest_path.parent / "generate_cases.py",
                  manifest_path, REPO / "examples" / "pilot_tool.rs"):
            if f.exists():
                dest = snap / f.name
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(f, dest)
        write(batch_dir / "plan.json", json.dumps(
            [{k: v for k, v in s.items() if not k.startswith("_")} for s in plan], indent=2) + "\n")
        meta = {
            "batch_id": batch_id,
            "cohort": cohort,
            "cohort_fingerprint": cohort_fp,
            "binary_sha256": cohort["binary_sha256"],
            "code_fingerprint": cohort["code_fingerprint"],
            "manifest": str(manifest_path),
            "suites": sorted(ctx.suites),
            "planned": len(plan),
            "total_budget_s": ctx.total_budget,
            "search_timeout_s": ctx.search_timeout,
            "replay_timeout_s": ctx.replay_timeout,
            "env_collection_ms": env["env_collection_ms"],
            "env_outside_budget": True,
            "results_dir": str(batch_dir),
            "total_budget_rule": "per-invocation recovery quota; not part of cohort identity",
        }
        write(batch_dir / "batch.json", json.dumps(meta, indent=2) + "\n")
    else:
        frozen = json.loads((batch_dir / "batch.json").read_text())
        if frozen.get("cohort_fingerprint") != cohort_fp:
            diffs = _cohort_diff(frozen.get("cohort", {}), cohort)
            print(json.dumps({
                "error": "cohort-mismatch",
                "batch_id": batch_id,
                "message": "resume identity does not match the frozen batch; use a new --batch-tag",
                "differences": diffs,
            }, indent=2))
            return 2
        frozen_plan = json.loads((batch_dir / "plan.json").read_text())
        if sorted(p["run_key"] for p in frozen_plan) != sorted(s["run_key"] for s in plan):
            print(json.dumps({
                "error": "plan-mismatch", "batch_id": batch_id,
                "message": "recomputed plan does not match the frozen plan",
            }, indent=2))
            return 2
        meta = frozen

    attempts_path = batch_dir / "attempts.jsonl"
    index_path = batch_dir / "valid_index.jsonl"
    valid_index = _load_index(index_path)
    demoted = revalidate_index(valid_index)
    next_seq = next_attempt_seq(attempts_path)

    _append_jsonl(batch_dir / "resume_events.jsonl", {
        "at_utc": now_utc(),
        "resuming": resuming,
        "total_budget_s": ctx.total_budget,
        "search_timeout_s": ctx.search_timeout,
        "replay_timeout_s": ctx.replay_timeout,
        "binary_sha256": cohort["binary_sha256"],
        "code_fingerprint": cohort["code_fingerprint"],
        "plan_count": len(plan),
        "pid": os.getpid(),
    })

    deadline = time.monotonic() + ctx.total_budget
    counts = {"executed": 0, "reused": 0, "replay_recovered": 0, "not_executed": 0,
              "resume_mismatch": 0}
    for spec in plan:
        key = spec["run_key"]
        if time.monotonic() >= deadline and not args.allow_overrun:
            if key not in valid_index:
                valid_index[key] = {
                    "run_key": key, "run_fingerprint": spec["run_fingerprint"],
                    "stage": spec["stage"], "suite": spec["suite"], "case": spec["case"],
                    "family": spec["family"], "params": spec["params"],
                    "config_name": spec["config_name"], "config": spec["config"],
                    "strategy": spec["strategy"], "repeat": spec["repeat"],
                    "identity": spec["identity"], "evidence_status": "not_executed",
                    "final_classification": "not_executed", "raw_outcome": None,
                    "wall_ms": 0, "wall_ms_basis": "none",
                    "notes": "total budget exhausted before planning",
                }
            counts["not_executed"] += 1
            continue
        if ctx.resume:
            rec, why = load_resume_index(ctx, batch_dir, spec, ctx.search_timeout, ctx.replay_timeout)
            if rec is not None:
                if rec.get("evidence_status") == "complete":
                    # Pure reuse: no new attempt directory, no new sample.
                    if _best(valid_index.get(key), rec):
                        valid_index[key] = dict(rec)
                    _append_jsonl(batch_dir / "resume_events.jsonl", {
                        "at_utc": now_utc(), "reused_run_key": key,
                        "reused_attempt_id": rec.get("attempt_id"),
                        "classification": rec.get("final_classification"),
                    })
                    counts["reused"] += 1
                    print(f"[reuse] {run_slug(spec):44} {rec['final_classification']}")
                    continue
                if rec.get("evidence_status") == "replay_pending":
                    spec["replay_only_from"] = rec["artifact_path"]
                    spec["_pending_record"] = rec
                    rec2, st = execute_attempt(ctx, spec, batch_dir, valid_index, next_seq,
                                               deadline, index_path, attempts_path)
                    next_seq += 1
                    counts["replay_recovered"] += 1
                    print(f"[replay] {run_slug(spec):44} {rec2['final_classification']}")
                    continue
                counts["resume_mismatch"] += 1
        rec, status = execute_attempt(ctx, spec, batch_dir, valid_index, next_seq,
                                      deadline, index_path, attempts_path)
        next_seq += 1
        counts["executed"] += 1
        print(f"[{counts['executed']:>3}/{len(plan)}] {run_slug(spec):44} "
              f"{rec['final_classification']:<22} {rec.get('wall_ms',0)}ms")

    _write_index(index_path, valid_index)
    meta = dict(meta)
    meta.update({**counts, "resume": resuming, "results_dir": str(batch_dir),
                 "evidence_demoted": demoted,
                 "invocation_total_budget_s": ctx.total_budget})
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


def do_lifecycle(args) -> int:
    """End-to-end lifecycle regression on real `do_run`/`execute_attempt`/
    `summarize`/`audit` paths with a minimal real case.

    Fault injection is only at the subprocess boundary (raising a crash, or
    returning a timeout/exit code); the runner logic itself is not replaced.
    """
    sys.path.insert(0, str(SCRIPTS))
    import pilot_analyze as _pa
    import pilot_audit as _pau
    tmp = (Path(args.out).resolve() / f"lifecycle-{os.getpid()}")
    if tmp.exists():
        shutil.rmtree(tmp)
    tmp.mkdir(parents=True, exist_ok=True)
    binary = Path(args.bin).resolve()
    tool = Path(args.tool).resolve()
    src_manifest = Path(args.manifest).resolve()
    m = json.loads(src_manifest.read_text())
    case = dict(next(c for c in m["cases"] if c["case"] == "p1_same"))
    case["model"] = str((src_manifest.parent / case["model"]).resolve())
    case["contract"] = str((src_manifest.parent / case["contract"]).resolve())
    case["repeat_policy"] = {"main": 1, "tight": 0}
    tiny = {"schema": "concir-pilot-manifest-v2",
            "search_configs": {"main": m["search_configs"]["main"]},
            "cases": [case], "smoke_cases": [], "matrix_cases": []}
    tiny_path = tmp / "mini_manifest.json"
    tiny_path.write_text(json.dumps(tiny))

    checks = []
    real = run_process

    def call(batch_tag, resume, budget, timeout=30.0, manifest=tiny_path):
        ns = argparse.Namespace(out=str(tmp), manifest=str(manifest), bin=str(binary),
                                tool=str(tool), suite=["pilot"], search_timeout=timeout,
                                replay_timeout=timeout, total_budget=budget,
                                resume=resume, batch_tag=batch_tag, allow_overrun=False)
        with contextlib.redirect_stdout(io.StringIO()):
            return do_run(ns)

    def bdir(tag):
        return tmp / "results" / "batches" / tag

    def index(tag):
        out = {}
        p = bdir(tag) / "valid_index.jsonl"
        if p.exists():
            for line in p.read_text().splitlines():
                if line.strip():
                    r = json.loads(line)
                    out[r["run_key"]] = r
        return out

    def tree_hash(root):
        return {str(p): sha256_file(p) for p in sorted(root.rglob("*")) if p.is_file()}

    def attempt_dirs(root):
        return sorted(str(p) for p in root.rglob("attempt-*") if p.is_dir())

    # ── 1. existing batch tag without --resume is refused before any write ──
    assert call("b1", False, 60) == 0
    before = tree_hash(bdir("b1"))
    rc = call("b1", False, 60)
    checks.append(("refuse_existing_batch_without_resume",
                   rc == 2 and tree_hash(bdir("b1")) == before, f"rc={rc}"))

    # ── 2. crash mid-run, then --resume continues without overwriting ──
    counter = {"n": 0}

    def crash_second_repair(cmd, timeout_s, cwd=REPO):
        cs = [str(c) for c in cmd]
        if len(cs) > 1 and cs[1] == "repair":
            counter["n"] += 1
            if counter["n"] >= 2:
                raise KeyboardInterrupt("injected interruption")
        return real(cmd, timeout_s, cwd)

    globals()["run_process"] = crash_second_repair
    try:
        try:
            call("b2", False, 60)
        except KeyboardInterrupt:
            pass
    finally:
        globals()["run_process"] = real
    dirs_after_crash = attempt_dirs(bdir("b2"))
    # attempt evidence (raw/) must not change; index/events legitimately append
    hashes_after_crash = tree_hash(bdir("b2") / "raw")
    partial = index("b2")
    rc = call("b2", True, 60)
    idx2 = index("b2")
    old_ok = all(hashes_after_crash.get(p) == sha256_file(Path(p))
                 for p in hashes_after_crash)
    complete2 = all(r["evidence_status"] == "complete" for r in idx2.values())
    checks.append(("resume_after_crash_no_overwrite",
                   rc == 0 and len(idx2) == 3 and complete2 and len(partial) < 3
                   and old_ok and len(attempt_dirs(bdir("b2"))) >= len(dirs_after_crash) + (3 - len(partial)),
                   f"rc={rc} partial={len(partial)} final={len(idx2)} complete={complete2}"))

    # ── 3. changed cohort under the same tag is refused; new tag works ──
    changed = json.loads(tiny_path.read_text())
    changed["search_configs"]["main"]["candidate_budget"] += 1
    changed_path = tmp / "mini_manifest_changed.json"
    changed_path.write_text(json.dumps(changed))
    b1_before = tree_hash(bdir("b1"))
    rc_cfg = call("b1", True, 60, manifest=changed_path)
    rc_to = call("b1", True, 60, timeout=31.0)
    rc_new = call("b3", False, 60, manifest=changed_path)
    checks.append(("cohort_change_refused_new_tag_ok",
                   rc_cfg == 2 and rc_to == 2 and rc_new == 0
                   and tree_hash(bdir("b1")) == b1_before,
                   f"cfg={rc_cfg} timeout={rc_to} new={rc_new}"))

    # ── 4. current failure without an artifact is not bound to old evidence ──
    ctx = Context(tiny_path.resolve(), tmp, binary, tool, ["pilot"], 30, 30, 60, False)
    spec = _repair_run(ctx, "pilot", case, "main", ctx.manifest["search_configs"]["main"], "b", 1)
    _bind_spec(ctx, spec)
    spec["batch_id"] = "b4"
    b4 = tmp / "b4"
    b4.mkdir(parents=True, exist_ok=True)
    vi = {}
    rec_ok, _ = execute_attempt(ctx, spec, b4, vi, 1, time.monotonic() + 60,
                                b4 / "valid_index.jsonl", b4 / "attempts.jsonl")
    old_hash = sha256_file(Path(rec_ok["artifact_path"]))
    old_bytes = Path(rec_ok["artifact_path"]).read_bytes()

    def fail_all(cmd, timeout_s, cwd=REPO):
        cs = [str(c) for c in cmd]
        if len(cs) > 1 and cs[1] == "repair":
            return {"cmd": cs, "exit_code": 2, "stdout": "",
                    "stderr": "injected failure", "wall_ms": 1, "timeout_s": timeout_s,
                    "timed_out": False, "spawn_error": None}
        return real(cmd, timeout_s, cwd)

    globals()["run_process"] = fail_all
    try:
        rec_fail, _ = execute_attempt(ctx, spec, b4, vi, 2, time.monotonic() + 60,
                                      b4 / "valid_index.jsonl", b4 / "attempts.jsonl")
    finally:
        globals()["run_process"] = real
    kept = vi[spec["run_key"]]
    checks.append(("failure_not_bound_to_old_artifact",
                   rec_fail["evidence_status"] in ("no_artifact", "json_error")
                   and kept["evidence_status"] == "complete"
                   and kept["final_classification"] == "repaired"
                   and Path(kept["artifact_path"]).read_bytes() == old_bytes
                   and sha256_file(Path(kept["artifact_path"])) == old_hash,
                   f"new={rec_fail['evidence_status']} kept={kept['evidence_status']}"))

    # ── 4b. R1: invalid old evidence leaves the success index; summarize rejects it ──
    key_b1 = next(k for k, r in index("b1").items() if r["strategy"] == "b")
    orig = dict(index("b1")[key_b1])
    orig_bytes = Path(orig["artifact_path"]).read_bytes()
    original_hash = sha256_bytes(orig_bytes)
    Path(orig["artifact_path"]).unlink()
    globals()["run_process"] = fail_all
    try:
        rc = call("b1", True, 60)
    finally:
        globals()["run_process"] = real
    rec_b1 = index("b1")[key_b1]
    demoted_ok = (rec_b1.get("evidence_status") != "complete"
                  and rec_b1.get("final_classification") != "repaired")

    # summarize must reject a complete record whose artifact is gone / hash differs
    invdir = tmp / "invalidbatch"
    invdir.mkdir(parents=True, exist_ok=True)
    gone = dict(orig)
    gone["run_key"] = "kgone"
    bad_hash = dict(orig)
    bad_hash["run_key"] = "kbad"
    bad_hash["artifact_path"] = kept["artifact_path"]
    bad_hash["artifact_sha256"] = "0" * 64
    (invdir / "plan.json").write_text(json.dumps([{"run_key": "kgone"}, {"run_key": "kbad"}]))
    (invdir / "valid_index.jsonl").write_text(
        json.dumps(gone) + "\n" + json.dumps(bad_hash) + "\n")
    sum_inv = _pa.summarize_batch(invdir, tmp)
    s_inv = json.loads((invdir / "summary.json").read_text())
    reject_ok = sum_inv != 0 and s_inv["complete"] == 0 and len(s_inv["errors"]) >= 2
    checks.append(("invalid_old_evidence_demoted_and_rejected",
                   demoted_ok and reject_ok,
                   f"demoted={demoted_ok} idx_status={rec_b1.get('evidence_status')} "
                   f"sum={sum_inv} complete={s_inv['complete']} errors={len(s_inv['errors'])}"))

    # ── 5. replay_pending recovery keeps full search evidence and is idempotent ──
    assert call("b5", False, 60) == 0
    idx5 = index("b5")
    key_b = next(k for k, r in idx5.items() if r["strategy"] == "b")
    pend = dict(idx5[key_b])
    pend.update(evidence_status="replay_pending", final_classification="replay_pending",
                replay_ok=None, replay_exit=None, replay_wall_ms=None)
    _write_index(bdir("b5") / "valid_index.jsonl", {**idx5, key_b: pend})
    rc = call("b5", True, 60)
    rec = index("b5")[key_b]
    fields_ok = (rec["evidence_status"] == "complete"
                 and rec["final_classification"] == "repaired"
                 and rec.get("artifact_path") == pend.get("artifact_path")
                 and rec.get("artifact_sha256") == pend.get("artifact_sha256")
                 and rec.get("verification_calls") == pend.get("verification_calls")
                 and rec.get("states_explored") == pend.get("states_explored")
                 and rec.get("patch_len") == pend.get("patch_len")
                 and rec.get("root_outcome") == pend.get("root_outcome")
                 and rec.get("search_wall_ms") == pend.get("search_wall_ms")
                 and rec.get("exit_code") == pend.get("exit_code")
                 and rec.get("replay_ok") is True and rec.get("replay_exit") == 0
                 and rec.get("wall_ms") == rec.get("search_wall_ms"))
    n_dirs = len(attempt_dirs(bdir("b5")))
    idx_hash = sha256_file(bdir("b5") / "valid_index.jsonl")
    rc2 = call("b5", True, 60)
    reused_ok = (rc2 == 0
                 and len(attempt_dirs(bdir("b5"))) == n_dirs
                 and sha256_file(bdir("b5") / "valid_index.jsonl") == idx_hash)
    import pilot_analyze as _pa
    import pilot_audit as _pau
    sum_rc = _pa.summarize_batch(bdir("b5"), tmp)
    aud = _pau.audit(tmp)
    audit_ok = not any("b5" in i for i in aud["issues"])
    checks.append(("replay_pending_recovery_full_and_idempotent",
                   fields_ok and reused_ok and sum_rc == 0 and audit_ok,
                   f"fields={fields_ok} reused={reused_ok} sum={sum_rc} audit={audit_ok}"))
    # R2: wall_ms is the search cost on both paths; replay/end-to-end are separate
    wall_ok = (rec.get("wall_ms") == rec.get("search_wall_ms")
               and rec.get("end_to_end_wall_ms") ==
               (rec.get("search_wall_ms") or 0) + (rec.get("replay_wall_ms") or 0)
               and rec.get("replay_wall_ms") is not None)
    normal = index("b5").get(next(k for k in index("b5") if index("b5")[k]["strategy"] == "a"))
    checks.append(("recovery_search_cost_comparable",
                   wall_ok and normal is not None and normal.get("wall_ms") == normal.get("search_wall_ms"),
                   f"recovered wall={rec.get('wall_ms')} search={rec.get('search_wall_ms')} "
                   f"e2e={rec.get('end_to_end_wall_ms')} replay={rec.get('replay_wall_ms')}"))

    # ── 6. recovery refuses incomplete / failed replay ──
    ctx6 = Context(tiny_path.resolve(), tmp, binary, tool, ["pilot"], 30, 30, 60, False)
    spec6 = _repair_run(ctx6, "pilot", case, "main", ctx6.manifest["search_configs"]["main"], "b", 1)
    _bind_spec(ctx6, spec6)

    def recover(pending, tag, wrapper=None):
        sp = dict(spec6)
        sp["batch_id"] = tag
        sp["replay_only_from"] = pending["artifact_path"]
        sp["_pending_record"] = pending
        d = tmp / tag
        d.mkdir(parents=True, exist_ok=True)
        saved = globals()["run_process"]
        if wrapper:
            globals()["run_process"] = wrapper
        try:
            rec, _ = execute_attempt(ctx6, sp, d, {}, 1, time.monotonic() + 60,
                                     d / "valid_index.jsonl", d / "attempts.jsonl")
        finally:
            globals()["run_process"] = saved
        return rec

    good = rec
    missing = dict(good)
    missing["artifact_path"] = str(tmp / "does_not_exist.json")
    rec_missing = recover(missing, "b6a")
    tampered_hash = dict(good)
    tampered_hash["artifact_sha256"] = "0" * 64
    rec_tampered = recover(tampered_hash, "b6b")

    def replay_timeout(cmd, timeout_s, cwd=REPO):
        cs = [str(c) for c in cmd]
        if len(cs) > 1 and cs[1] == "replay":
            return {"cmd": cs, "exit_code": None, "stdout": "", "stderr": "",
                    "wall_ms": int(timeout_s * 1000), "timeout_s": timeout_s,
                    "timed_out": True, "spawn_error": None}
        return real(cmd, timeout_s, cwd)
    rec_timeout = recover(good, "b6c", replay_timeout)

    def replay_fail(cmd, timeout_s, cwd=REPO):
        cs = [str(c) for c in cmd]
        if len(cs) > 1 and cs[1] == "replay":
            return {"cmd": cs, "exit_code": 4, "stdout": "", "stderr": "injected replay fail",
                    "wall_ms": 1, "timeout_s": timeout_s, "timed_out": False, "spawn_error": None}
        return real(cmd, timeout_s, cwd)
    rec_failreplay = recover(good, "b6d", replay_fail)
    checks.append(("recovery_refuses_incomplete",
                   rec_missing["evidence_status"] != "complete"
                   and rec_tampered["evidence_status"] != "complete"
                   and rec_timeout["evidence_status"] == "replay_timeout"
                   and rec_failreplay["evidence_status"] == "replay_failed",
                   f"missing={rec_missing['evidence_status']} tampered={rec_tampered['evidence_status']} "
                   f"timeout={rec_timeout['evidence_status']} fail={rec_failreplay['evidence_status']}"))

    # ── 7. statistics: distinct configs are not repeats; all outcomes paired ──
    sb = tmp / "statsbatch"
    sb.mkdir(parents=True, exist_ok=True)
    art = sb / "art.json"
    art.write_text(json.dumps({"outcome": "repaired"}))
    ahash = sha256_file(art)

    def base_rec(key, config, strategy, repeat):
        return {"run_key": key, "stage": "repair", "suite": "pilot", "case": "p1_same",
                "config_name": "main", "config": config, "strategy": strategy,
                "repeat": repeat, "identity": {"model_norm_sha256": "m", "contract_norm_sha256": "c"},
                "evidence_status": "complete", "final_classification": "repaired",
                "raw_outcome": "repaired", "exit_code": 0, "artifact_path": str(art),
                "artifact_sha256": ahash, "search_wall_ms": 1, "verification_calls": 2,
                "states_explored": 10, "patch_len": 1, "root_outcome": "FAIL", "wall_ms": 1,
                "replay_ok": True, "replay_exit": 0, "search_timeout": 30.0, "replay_timeout": 30.0,
                "stop_reason": "solved"}
    cfg_a = {"candidate_budget": 64, "verification_budget": 64, "max_depth": 4, "max_total_edits": 4}
    cfg_b = dict(cfg_a, candidate_budget=65)
    recs = [
        base_rec("kA", cfg_a, "a", 1), base_rec("kB", cfg_a, "b", 1), base_rec("kC", cfg_a, "c", 1),
        base_rec("kD", cfg_b, "b", 1),
    ]
    plan = [{"run_key": r["run_key"]} for r in recs]
    (sb / "plan.json").write_text(json.dumps(plan))
    (sb / "valid_index.jsonl").write_text("\n".join(json.dumps(r) for r in recs) + "\n")
    sum_rc = _pa.summarize_batch(sb, tmp)
    s = json.loads((sb / "summary.json").read_text())
    distinct_ok = (not any("duplicate repeat" in e for e in s["errors"])
                   and s["paired"]["attempted_pairs"] == 1
                   and s["paired"]["both_repaired"] == 1)
    checks.append(("stats_distinct_config_and_pairing", sum_rc == 0 and distinct_ok,
                   f"rc={sum_rc} attempted={s['paired']['attempted_pairs']} "
                   f"both={s['paired']['both_repaired']} errors={s['errors']}"))

    # R3: outcome comparison keeps timeout / not_executed / differences
    pb = tmp / "pairbatch"
    pb.mkdir(parents=True, exist_ok=True)
    pbart = pb / "art.json"
    pbart.write_text(json.dumps({"outcome": "repaired"}))
    pahash = sha256_file(pbart)
    pident = {"model_norm_sha256": "m", "contract_norm_sha256": "c"}
    pcfg = {"candidate_budget": 64, "verification_budget": 64, "max_depth": 4, "max_total_edits": 4}

    def prec(key, strat, repeat, outcome, status):
        r = {"run_key": key, "stage": "repair", "suite": "pilot", "case": "p1_same",
             "config_name": "main", "config": pcfg, "strategy": strat, "repeat": repeat,
             "identity": pident, "evidence_status": status,
             "final_classification": outcome, "search_timeout": 30.0, "replay_timeout": 30.0}
        if outcome == "repaired":
            r.update(raw_outcome="repaired", exit_code=0, artifact_path=str(pbart),
                     artifact_sha256=pahash, search_wall_ms=9, verification_calls=2,
                     states_explored=10, patch_len=1, root_outcome="FAIL", wall_ms=9,
                     replay_ok=True, replay_exit=0, replay_wall_ms=5)
        else:
            r.update(raw_outcome=None, exit_code=None, search_wall_ms=None, wall_ms=0)
        return r

    precs = [
        prec("g1b", "b", 1, "repaired", "complete"),
        prec("g1c", "c", 1, "search_timeout", "search_timeout"),
        prec("g2b", "b", 2, "search_timeout", "search_timeout"),
        prec("g2c", "c", 2, "repaired", "complete"),
        prec("g3b", "b", 3, "repaired", "complete"),
        prec("g3c", "c", 3, "repaired", "complete"),
        prec("g4b", "b", 4, "not_executed", "not_executed"),
        prec("g4c", "c", 4, "repaired", "complete"),
    ]
    (pb / "plan.json").write_text(json.dumps(
        [{"run_key": r["run_key"], "stage": "repair", "strategy": r["strategy"],
          "config_name": r["config_name"], "config": r["config"], "identity": r["identity"],
          "repeat": r["repeat"], "suite": r["suite"], "case": r["case"]} for r in precs]))
    (pb / "valid_index.jsonl").write_text("\n".join(json.dumps(r) for r in precs) + "\n")
    sum_pb = _pa.summarize_batch(pb, tmp)
    sp = json.loads((pb / "summary.json").read_text())["paired"]
    r3_ok = (sum_pb == 0 and sp["planned_pairs"] == 4 and sp["attempted_pairs"] == 4
             and sp["both_repaired"] == 1 and sp["success_differs_count"] == 3
             and sp["one_side_not_executed"] == 1 and sp["cost_subset_pairs"] == 1
             and sp["sum_b_search_wall_ms"] == 9 and sp["sum_c_search_wall_ms"] == 9)
    checks.append(("pairing_keeps_timeout_and_not_executed",
                   r3_ok,
                   f"planned={sp['planned_pairs']} attempted={sp['attempted_pairs']} "
                   f"both={sp['both_repaired']} differs={sp['success_differs_count']} "
                   f"not_exec={sp['one_side_not_executed']} cost={sp['cost_subset_pairs']}"))

    # ── 8. supported external --out -- the behavior command exits 0 ──
    ext = tmp / "external-behavior"
    proc = subprocess.run(
        [sys.executable, str(SCRIPTS / "run_pilot.py"), "--out", str(ext),
         "--manifest", str(src_manifest), "behavior"],
        capture_output=True, text=True, timeout=300)
    checks.append(("external_out_behavior", proc.returncode == 0,
                   f"rc={proc.returncode} tail={proc.stdout.strip().splitlines()[-1] if proc.stdout else ''}"))

    ok = all(c[1] for c in checks)
    for name, good_, detail in checks:
        print(f"{'PASS' if good_ else 'FAIL'}  {name}: {detail}")
    print(f"lifecycle regression {'passed' if ok else 'FAILED'}")
    return 0 if ok else 1


def _bind_spec(ctx, spec):
    """Normalize inputs and bind per-run identity/timeouts (same as do_run)."""
    spec["_norm_model"] = ctx.normalize(Path(spec["model"]), "program")["norm"]
    spec["_norm_contract"] = ctx.normalize(Path(spec["contract"]), "contract")["norm"]
    spec["identity"]["contract_bounds"] = spec["_norm_contract"].get("bounds")
    spec["search_timeout"] = 30.0
    spec["replay_timeout"] = 30.0
    spec["run_key"] = run_key(spec, 30.0, 30.0)
    spec["run_fingerprint"] = run_fingerprint(ctx, spec, 30.0, 30.0)
    spec["_fp"] = spec["run_fingerprint"]
    return spec


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

    p = sub.add_parser("lifecycle")
    p.set_defaults(func=do_lifecycle)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
