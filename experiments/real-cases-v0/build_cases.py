#!/usr/bin/env python3
"""Build the real-cases-v0 CIR models and runner manifest.

The models here are hand-written reductions of upstream concurrency defects.
They are NOT claimed to be complete semantic equivalents of the source; see each
case's `mapping.md` for the exact abstraction and its limits. Ground truth comes
from the upstream issue/reproducer, never from this tool's output.

Run: python3 experiments/real-cases-v0/build_cases.py
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CASES = ROOT / "cases"
SOURCES = ROOT / "sources"

BOUNDS = {
    "max_threads": 8,
    "max_frames_per_thread": 8,
    "max_states": 20000,
    "max_depth": 64,
    "max_boundary_events": 256,
}
MAIN_CFG = {"candidate_budget": 64, "verification_budget": 64, "max_depth": 4, "max_total_edits": 4}
TIGHT_CFG = {"candidate_budget": 8, "verification_budget": 4, "max_depth": 1, "max_total_edits": 1}


def sha256_file(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def resource(name, kind="sync", rtype="Mutex"):
    return {"name": name, "kind": kind, "type": rtype, "mode": "Sync"}


def lock(sid, r):
    return {"sid": sid, "kind": "mutex_lock", "resource": r}


def unlock(sid, r):
    return {"sid": sid, "kind": "mutex_unlock", "resource": r}


def rw_read(sid, r):
    return {"sid": sid, "kind": "rwlock_read", "resource": r}


def rw_write(sid, r):
    return {"sid": sid, "kind": "rwlock_write", "resource": r}


def rw_unlock(sid, r):
    return {"sid": sid, "kind": "rwlock_unlock", "resource": r}


def ret(sid):
    return {"sid": sid, "kind": "return"}


def fn(name, body):
    return {"name": name, "kind": "normal", "form": "closure", "body": body}


def module(name, resources, functions):
    return {
        "name": name,
        "provides": {"resources": [r["name"] for r in resources],
                     "functions": [f["name"] for f in functions]},
        "requires": {"resources": [], "functions": []},
        "resources": resources,
        "protection": [],
        "functions": functions,
    }


def program(name, resources, functions):
    return {"program": name, "version": "3.5.0", "entry": "main::main",
            "modules": [module("main", resources, functions)]}


def contract(name, preserved_fqns, allowed_scope=None):
    return {
        "name": name,
        "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
        "preserved": [
            {"kind": "reachable", "description": f"{f} completes",
             "goal": {"kind": "function_completed", "function": f}}
            for f in preserved_fqns
        ],
        "assumptions": {"sequential_consistency": True, "no_spurious_wakeups": True},
        "bounds": dict(BOUNDS),
        "allowed_scope": allowed_scope or {"allow_lock_reorder": True},
    }


# ── case 1: ros2/rmw_zenoh #998 (std::mutex ABBA) ──────────────────────────
# Path A (executor rmw_wait): waitset condition_mutex -> subscription mutex_
# Path B (rx callback):      subscription mutex_ -> waitset condition_mutex
def build_rmw_zenoh():
    wait = resource("wait_set_condition_mutex")
    sub = resource("subscription_mutex")
    funcs = [
        fn("main", [{"sid": "s1", "kind": "scope",
                     "funcs": ["executor_wait", "rx_callback"]}, ret("s2")]),
        fn("executor_wait", [lock("s1", "wait_set_condition_mutex"),
                             lock("s2", "subscription_mutex"),
                             unlock("s3", "subscription_mutex"),
                             unlock("s4", "wait_set_condition_mutex"), ret("s5")]),
        fn("rx_callback", [lock("s1", "subscription_mutex"),
                           lock("s2", "wait_set_condition_mutex"),
                           unlock("s3", "wait_set_condition_mutex"),
                           unlock("s4", "subscription_mutex"), ret("s5")]),
    ]
    buggy = program("rmw_zenoh_998", [wait, sub], funcs)
    # Hypothesis fix: make rx_callback acquire in the same order as executor_wait
    # (adjacent swap of its two lock statements). The upstream suggested fix is
    # different (release mutex_ before taking condition_mutex); see mapping.md.
    fixed_funcs = json.loads(json.dumps(funcs))
    fixed_funcs[2]["body"] = [lock("s1", "wait_set_condition_mutex"),
                              lock("s2", "subscription_mutex"),
                              unlock("s3", "subscription_mutex"),
                              unlock("s4", "wait_set_condition_mutex"), ret("s5")]
    fixed = program("rmw_zenoh_998_fixed", [wait, sub], fixed_funcs)
    return buggy, fixed, contract("rmw-zenoh-998", ["main::executor_wait", "main::rx_callback"])


# ── case 2: xacrimon/dashmap #369 (per-shard RwLock ABBA) ──────────────────
# Thread 0: read(shard_A) then write(shard_B); Thread 1: read(shard_B) then
# write(shard_A). RwLock ops are represented faithfully; CIR v1 lowering marks
# them unsupported, which is itself the recorded outcome.
def build_dashmap():
    a = resource("shard_alpha", "sync", "RwLock")
    b = resource("shard_beta", "sync", "RwLock")
    funcs = [
        fn("main", [{"sid": "s1", "kind": "scope", "funcs": ["t0", "t1"]}, ret("s2")]),
        fn("t0", [rw_read("s1", "shard_alpha"), rw_write("s2", "shard_beta"),
                  rw_unlock("s3", "shard_beta"), rw_unlock("s4", "shard_alpha"), ret("s5")]),
        fn("t1", [rw_read("s1", "shard_beta"), rw_write("s2", "shard_alpha"),
                  rw_unlock("s3", "shard_alpha"), rw_unlock("s4", "shard_beta"), ret("s5")]),
    ]
    buggy = program("dashmap_369", [a, b], funcs)
    return buggy, None, contract("dashmap-369", ["main::t0", "main::t1"])


def write_case(d, name, buggy, fixed, contract_obj, provenance, mapping, expected):
    case_dir = CASES / name
    case_dir.mkdir(parents=True, exist_ok=True)
    (case_dir / "buggy.cir.json").write_text(json.dumps(buggy, indent=2) + "\n")
    if fixed is not None:
        (case_dir / "fixed.cir.json").write_text(json.dumps(fixed, indent=2) + "\n")
    (case_dir / "contract.json").write_text(json.dumps(contract_obj, indent=2) + "\n")
    provenance = dict(provenance)
    snippet = (ROOT / provenance["snippet_path"]).resolve()
    if snippet.exists():
        provenance["snippet_sha256"] = sha256_file(snippet)
    (case_dir / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    (case_dir / "mapping.md").write_text(mapping)
    expected["model_sha256"] = sha256_file(case_dir / "buggy.cir.json")
    expected["contract_sha256"] = sha256_file(case_dir / "contract.json")
    (case_dir / "expected.json").write_text(json.dumps(expected, indent=2) + "\n")


def main():
    CASES.mkdir(parents=True, exist_ok=True)
    SOURCES.mkdir(parents=True, exist_ok=True)

    rmw = build_rmw_zenoh()
    write_case(
        ROOT, "rmw-zenoh-998", *rmw,
        provenance={
            "id": "rmw-zenoh-998",
            "source_kind": "upstream issue with gdb proof",
            "language": "C++ (ROS 2 rmw_zenoh); the synchronized primitives are std::mutex",
            "repository": "https://github.com/ros2/rmw_zenoh",
            "issue": "https://github.com/ros2/rmw_zenoh/issues/998",
            "buggy_ref": "rolling @ e95c62d (reported against v0.11.0; package 0.2.9-1noble)",
            "buggy_lines": [
                "rmw_zenoh.cpp:2240 unique_lock(condition_mutex)",
                "rmw_zenoh.cpp:2157 -> rmw_subscription_data.cpp:916 lock(mutex_)",
                "rmw_subscription_data.cpp:1114 lock(mutex_)",
                "detail/event.cpp:263 lock(condition_mutex)",
            ],
            "fixed_ref": None,
            "fixed_note": "Issue closed 2026 with no linked PR/commit exposed in the "
                          "issue; the stated fix direction is to release subscription "
                          "mutex_ before acquiring the wait set condition_mutex.",
            "license": "Apache-2.0",
            "snippet_path": "sources/rmw-zenoh-998.md",
            "retrieved_utc": "2026-09-18",
        },
        mapping="""# Mapping: ros2/rmw_zenoh #998 -> CIR

## Source mechanism (from the issue, not from this tool)
Classic ABBA on two `std::mutex`:
- Path A `rmw_wait`/`check_and_attach_condition`: waitset `condition_mutex` ->
  subscription `mutex_` (`rmw_zenoh.cpp:2240` then `.../rmw_subscription_data.cpp:916`).
- Path B `SubscriptionData::add_new_message` (zenoh RX callback): subscription
  `mutex_` -> waitset `condition_mutex` (`.../rmw_subscription_data.cpp:1114`,
  then `detail/event.cpp:263`).

## CIR correspondence
| source | CIR |
| --- | --- |
| executor thread `rmw_wait` | function `main::executor_wait` in the `main::main` scope |
| zenoh RX callback thread | function `main::rx_callback` in the same scope |
| wait set `condition_mutex` | resource `main::wait_set_condition_mutex` (sync Mutex) |
| subscription `mutex_` | resource `main::subscription_mutex` (sync Mutex) |
| Path A lock order | `executor_wait`: lock wait, lock sub, unlock sub, unlock wait |
| Path B lock order | `rx_callback`: lock sub, lock wait, unlock wait, unlock sub |
| `preserved` | both functions complete (reachability) |
| safety | `deadlock_free` |

## Omitted / abstracted
- All payload, queue, condvar, event and lifetime logic is omitted; only the two
  lock acquisitions and their release points are kept.
- The interval between the two locks is collapsed, so they are adjacent in the
  model. In the source they are separated by `check_and_attach_condition` /
  event-notification calls. This makes the model *more* susceptible to the
  adjacent-swap operator than faithful source-level reconstruction would be.
- `condition_variable.wait` releases `condition_mutex`; the model does not model
  condvars, so it over-approximates the window in which the waitset lock is held.
- Threads are spawned together and joined; scheduler fairness and priorities are
  not modelled.

## Ground truth
The defect (a genuine 2-cycle) comes from the upstream gdb evidence: "30792 holds
WAITSET, wants SUB; 30744 holds SUB, wants WAITSET". The reduction preserves the
two locks and their opposite acquisition order, so the deadlock is expected.

## Fix note
The issue's suggested fix (release `mutex_` before taking `condition_mutex`) is
*not* an adjacent lock swap and is not represented. The `fixed.cir.json` here is a
hypothesis model that unifies the lock order; whether the search can produce it
is tool evidence, not the upstream patch.
""",
        expected={
            "defect_representable": True,
            "root_verification_expected": "FAIL (upstream gdb ABBA proof)",
            "verification_completable": True,
            "current_patch_space_fixable_expected": True,
            "upstream_fix": "release subscription mutex_ before acquiring the wait set "
                            "condition_mutex (not an adjacent lock swap)",
            "ground_truth_source": "https://github.com/ros2/rmw_zenoh/issues/998",
        },
    )

    dash = build_dashmap()
    write_case(
        ROOT, "dashmap-369", *dash,
        provenance={
            "id": "dashmap-369",
            "source_kind": "upstream issue with minimal reproducer",
            "language": "Rust (per-shard RwLock)",
            "repository": "https://github.com/xacrimon/dashmap",
            "issue": "https://github.com/xacrimon/dashmap/issues/369",
            "buggy_ref": "dashmap 6.1.0 (issue reports version 6.1.0; no commit pinned)",
            "buggy_lines": [
                "get() returns a Ref holding a shard read lock",
                "insert() takes a different shard's write lock",
            ],
            "fixed_ref": None,
            "fixed_note": "Issue closed with no linked PR/commit exposed; the suggested "
                          "fix is to drop the Ref before mutating another key.",
            "license": "MIT",
            "snippet_path": "sources/dashmap-369.md",
            "retrieved_utc": "2026-09-18",
        },
        mapping="""# Mapping: xacrimon/dashmap #369 -> CIR

## Source mechanism (from the issue, not from this tool)
Per-shard `RwLock`. Thread 0 holds `read(shard_A)` (a `Ref` from `get("alpha")`)
and calls `insert("beta")`, which takes `write(shard_B)`. Thread 1 holds
`read(shard_B)` and calls `insert("alpha")` -> `write(shard_A)`. Under a schedule
where both reads are taken before both writes, each thread blocks on the other's
write lock: a 2-cycle.

## CIR correspondence
| source | CIR |
| --- | --- |
| reader/inserter thread 0 | function `main::t0` |
| reader/inserter thread 1 | function `main::t1` |
| shard containing `alpha` | resource `main::shard_alpha` (sync **RwLock**) |
| shard containing `beta` | resource `main::shard_beta` (sync RwLock) |
| `get()` guard | `rwlock_read` ... `rwlock_unlock` |
| `insert()` | `rwlock_write` ... `rwlock_unlock` |
| `preserved` | both threads complete |
| safety | `deadlock_free` |

## Omitted / abstracted
- Hashing / shard-selection is abstracted to two named shards; the number of
  shards, table resizing and `Ref`/`RefMut` lifetimes are omitted.
- Only one read guard and one write per thread are kept.
- RwLock read/write preference and reader-writer upgrade are not modelled.

## Ground truth and support boundary
The two-shard opposite-order cycle is taken from the issue's DPOR event trace.
However, **CIR v1 lowerer marks RwLock operations unsupported**
(`src/sem/program.rs`: "RwLock operations are not supported in v1"), so this
faithful model is expected to be `UNSUPPORTED`, not verified. It is included to
document a concrete support boundary rather than being re-wired to `Mutex` (which
would erase the read/write semantics the source depends on).
""",
        expected={
            "defect_representable": True,
            "root_verification_expected": "UNSUPPORTED (RwLock lowering unsupported in v1)",
            "verification_completable": False,
            "current_patch_space_fixable_expected": False,
            "upstream_fix": "drop the Ref guard before mutating another key "
                            "(not representable in the current patch/verification space)",
            "ground_truth_source": "https://github.com/xacrimon/dashmap/issues/369",
        },
    )

    # runner manifest: root verification + A/B/C feasibility (1 repeat each)
    manifest = {
        "schema": "concir-pilot-manifest-v2",
        "note": "real-cases-v0 feasibility run; ground truth from upstream issues",
        "search_configs": {"main": MAIN_CFG, "tight": TIGHT_CFG},
        "cases": [
            {"case": "rmw-zenoh-998", "family": "real_std_mutex_abba",
             "model": "cases/rmw-zenoh-998/buggy.cir.json",
             "contract": "cases/rmw-zenoh-998/contract.json",
             "params": {"source": "ros2/rmw_zenoh#998", "language": "C++/std::mutex"},
             "repeat_policy": {"main": 1, "tight": 0}},
            {"case": "dashmap-369", "family": "real_rwlock_abba_unsupported",
             "model": "cases/dashmap-369/buggy.cir.json",
             "contract": "cases/dashmap-369/contract.json",
             "params": {"source": "xacrimon/dashmap#369", "language": "Rust/RwLock"},
             "repeat_policy": {"main": 1, "tight": 0}},
        ],
        "matrix_cases": [
            {"case": "rmw-zenoh-998-buggy", "family": "real_root",
             "model": "cases/rmw-zenoh-998/buggy.cir.json",
             "params": {"variant": "buggy"},
             "variants": [{"label": "buggy", "contract": "cases/rmw-zenoh-998/contract.json",
                           "engine": "petri", "max_states": 20000}]},
            {"case": "rmw-zenoh-998-fixed", "family": "real_root",
             "model": "cases/rmw-zenoh-998/fixed.cir.json",
             "params": {"variant": "fixed_hypothesis"},
             "variants": [{"label": "fixed", "contract": "cases/rmw-zenoh-998/contract.json",
                           "engine": "petri", "max_states": 20000}]},
            {"case": "dashmap-369-buggy", "family": "real_root",
             "model": "cases/dashmap-369/buggy.cir.json",
             "params": {"variant": "buggy"},
             "variants": [{"label": "buggy", "contract": "cases/dashmap-369/contract.json",
                           "engine": "petri", "max_states": 20000}]},
        ],
        "smoke_cases": [],
    }
    (ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("wrote real-cases-v0 models and manifest")


if __name__ == "__main__":
    main()
