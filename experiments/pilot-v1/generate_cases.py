#!/usr/bin/env python3
"""Generate the deterministic pilot-v1 case set for the ConcIR repair loop.

This is a *generator of development/pilot data*, not a held-out evaluation set.
Every case is a small mutex program with a known construction:

  * a "defect unit" is a pair of functions that acquire the same two mutexes in
    opposite order (a lock-order cycle);
  * an "interference pair" is a benign, reachable function that acquires two
    *other* mutexes in one consistent order, adding extra swap candidates that
    are unrelated to the defect;
  * `cross` distributes the units over two modules (`main` and `other`);
  * the controls are an already-correct program and two programs whose defect is
    outside the contract's edit scope (so no acceptable candidate exists), plus
    one whose frozen analysis bounds are too small (so it must stay UNKNOWN).

For each case the generator also writes a "witness" program in which every
defect unit's two functions acquire their pair in the same order. The witness is
validated independently of the search strategies (see validate in run_pilot.py
and PILOT_HANDOFF.md).

Run:  python3 experiments/pilot-v1/generate_cases.py
"""

from __future__ import annotations

import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CASES = ROOT / "cases"
WITNESS = ROOT / "witness"
MANIFEST = ROOT / "manifest.json"

VERSION = "3.5.0"
MAIN_BOUNDS = {
    "max_threads": 16,
    "max_frames_per_thread": 16,
    "max_states": 200000,
    "max_depth": 128,
    "max_boundary_events": 512,
}
UNKNOWN_BOUNDS = dict(MAIN_BOUNDS, max_states=50)
# Three independent defect units (six workers) already need >1e5 states to
# analyse completely. The pilot freezes a smaller bound for those cases and
# keeps the resulting UNKNOWN rather than letting a single verification dominate
# the run budget.
P3_BOUNDS = dict(MAIN_BOUNDS, max_states=20000)


def resource(name: str) -> dict:
    return {"name": name, "kind": "sync", "type": "Mutex", "mode": "Sync"}


def lock(sid: str, r: str) -> dict:
    return {"sid": sid, "kind": "mutex_lock", "resource": r}


def unlock(sid: str, r: str) -> dict:
    return {"sid": sid, "kind": "mutex_unlock", "resource": r}


def ret(sid: str) -> dict:
    return {"sid": sid, "kind": "return"}


def lock_pair_body(r1: str, r2: str) -> list[dict]:
    """Acquire r1 then r2, release in reverse order, return."""
    return [
        lock("s1", r1),
        lock("s2", r2),
        unlock("s3", r2),
        unlock("s4", r1),
        ret("s5"),
    ]


def fn(name: str, body: list[dict]) -> dict:
    return {"name": name, "kind": "normal", "form": "closure", "body": body}


def main_fn(spawn: list[str]) -> dict:
    return fn("main", [{"sid": "s1", "kind": "scope", "funcs": spawn}, ret("s2")])


def module(name: str, res: list[str], funcs: list[dict], requires_funcs=None) -> dict:
    requires_funcs = requires_funcs or []
    return {
        "name": name,
        "provides": {"resources": list(res), "functions": [f["name"] for f in funcs]},
        "requires": {"resources": [], "functions": list(requires_funcs)},
        "resources": [resource(r) for r in res],
        "protection": [],
        "functions": funcs,
    }


def build(units, interference, name_order=False, module_order=False, unspawned=()):
    """Return (program_dict, witness_program_dict, all_function_fqns).

    units: list of (module, r1, r2). Each unit contributes two functions:
      <mod>::<u>_ab  acquires r1->r2
      <mod>::<u>_ba  acquires r2->r1
    interference: list of (module, r1, r2) benign pairs (single consistent order).
    """
    mods: dict[str, dict] = {}
    order: list[str] = []

    def ensure(mod: str):
        if mod not in mods:
            mods[mod] = {"res": [], "funcs": [], "requires": []}
            order.append(mod)

    for mod, _, _ in units + interference:
        ensure(mod)
    ensure("main")

    def add_res(mod: str, r: str):
        if r not in mods[mod]["res"]:
            mods[mod]["res"].append(r)

    def add_fn(mod: str, f: dict):
        mods[mod]["funcs"].append(f)

    defect_fns: list[tuple[str, str, str]] = []  # (mod, ab_fn, ba_fn)

    for i, (mod, r1, r2) in enumerate(units, start=1):
        add_res(mod, r1)
        add_res(mod, r2)
        ab = f"u{i}ab"
        ba = f"u{i}ba"
        add_fn(mod, fn(ab, lock_pair_body(r1, r2)))
        add_fn(mod, fn(ba, lock_pair_body(r2, r1)))
        defect_fns.append((mod, ab, ba))

    for j, (mod, r1, r2) in enumerate(interference, start=1):
        add_res(mod, r1)
        add_res(mod, r2)
        add_fn(mod, fn(f"i{j}", lock_pair_body(r1, r2)))

    # Declared but never spawned: used for a preserved goal that no lock edit can
    # make reachable.
    for gname in unspawned:
        add_fn("main", fn(gname, [ret("s1")]))

    # main's scope spawns every worker (including the other module's).
    spawn: list[str] = []
    for mod in order:
        for f in mods[mod]["funcs"]:
            if f["name"] == "main" or f["name"] in unspawned:
                continue
            fq = f"{mod}::{f['name']}" if mod != "main" else f["name"]
            if mod != "main":
                mods["main"]["requires"].append(fq)
            spawn.append(fq)

    main_mod = "main"
    ensures = mods.setdefault(main_mod, {"res": [], "funcs": [], "requires": []})
    if main_mod not in order:
        order.insert(0, main_mod)
    # Put main's function first in its module.
    main_func = main_fn(spawn)
    ensures["funcs"].insert(0, main_func)

    if name_order:
        # Reverse the worker order inside each module (main stays first).
        for mod in mods:
            fns = mods[mod]["funcs"]
            if fns and fns[0]["name"] == "main":
                fns[1:] = list(reversed(fns[1:]))
            else:
                fns[:] = list(reversed(fns))

    module_order_list = list(order)
    if module_order:
        # main last; entry still main::main.
        module_order_list = [m for m in module_order_list if m != "main"] + [
            m for m in module_order_list if m == "main"
        ]

    program = {
        "program": "pilot",
        "version": VERSION,
        "entry": "main::main",
        "modules": [
            module(m, mods[m]["res"], mods[m]["funcs"], mods[m]["requires"])
            for m in module_order_list
        ],
    }

    # Witness: the ba function of every defect unit acquires the same order as
    # its ab counterpart; interference and scope references are unchanged.
    pair_of = {}
    for i, (mod, r1, r2) in enumerate(units, start=1):
        pair_of[f"u{i}ba"] = (r1, r2)
    witness_modules = []
    for m in module_order_list:
        wfuncs = []
        for f in mods[m]["funcs"]:
            if f["name"] in pair_of:
                r1, r2 = pair_of[f["name"]]
                wfuncs.append(fn(f["name"], lock_pair_body(r1, r2)))
            else:
                wfuncs.append(json.loads(json.dumps(f)))
        witness_modules.append(module(m, mods[m]["res"], wfuncs, mods[m]["requires"]))
    witness_program = {
        "program": "pilot_witness",
        "version": VERSION,
        "entry": "main::main",
        "modules": witness_modules,
    }

    fqns = []
    for m in module_order_list:
        for f in mods[m]["funcs"]:
            if f["name"] == "main":
                continue
            fqns.append(f"{m}::{f['name']}" if m != "main" else f["name"])
    return program, witness_program, fqns


def contract(name, preserved_fqns, bounds=None, functions=None, allow_lock_reorder=True):
    preserved = [
        {"kind": "reachable", "description": f"{f} completes", "goal": {"kind": "function_completed", "function": f}}
        for f in preserved_fqns
    ]
    scope = {"allow_lock_reorder": allow_lock_reorder}
    if functions is not None:
        scope["functions"] = functions
    return {
        "name": name,
        "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
        "preserved": preserved,
        "assumptions": {"sequential_consistency": True, "no_spurious_wakeups": True},
        "bounds": bounds or MAIN_BOUNDS,
        "allowed_scope": scope,
    }


def case_def(
    case_id,
    family,
    units,
    interference=(),
    cross=False,
    name_order=False,
    module_order=False,
    bounds=None,
    scope_functions=None,
    allow_lock_reorder=True,
    expectation="",
    witness_scope_note="",
    unspawned=(),
    witness_expected="PASS",
):
    program, witness, fqns = build(
        list(units), list(interference), name_order, module_order, list(unspawned)
    )
    program["program"] = case_id
    witness["program"] = case_id + "_witness"
    cid = case_id
    model_path = f"cases/{cid}.json"
    witness_path = f"witness/{cid}_witness.json"
    contract_path = f"cases/{cid}_contract.json"
    n_defects = len(units)
    return {
        "case": case_id,
        "family": family,
        "model": model_path,
        "contract": contract_path,
        "witness": witness_path,
        "params": {
            "defect_units": n_defects,
            "interference_pairs": len(interference),
            "cross_module": cross,
            "name_order_permuted": name_order,
            "module_order_permuted": module_order,
        },
        "origin": "generated by experiments/pilot-v1/generate_cases.py",
        "expected_note": expectation,
        "known_witness_note": witness_scope_note
        or (
            "witness unifies each defect pair's lock order; independently verified "
            "against the frozen contract with both engines"
            if n_defects
            else "control: already correct, no repair needed"
        ),
        "minimal_patch_note": (
            f"{n_defects} independent disjoint lock-order pairs; each needs at least "
            "one adjacent swap, so the construction's minimum patch length is "
            f"{n_defects} (argument, not a search result)"
            if n_defects
            else "0"
        ),
        "program": program,
        "witness_program": witness,
        "preserved_fqns": fqns,
        "bounds": bounds or MAIN_BOUNDS,
        "scope_functions": scope_functions,
        "allow_lock_reorder": allow_lock_reorder,
        "witness_expected": witness_expected,
    }


def main():
    CASES.mkdir(parents=True, exist_ok=True)
    WITNESS.mkdir(parents=True, exist_ok=True)

    cases = [
        case_def("p1_same", "one_defect", [("main", "a", "b")],
                 expectation="constructed: one lock-order cycle in module main"),
        case_def("p1_cross", "one_defect", [("main", "a", "b")], cross=True,
                 expectation="constructed: one cycle split across main and other",
                 witness_scope_note="witness unifies main::u1ba; independently verified with both engines"),
        case_def("p2_same", "two_defects", [("main", "a", "b"), ("main", "c", "d")],
                 expectation="constructed: two independent cycles in main"),
        case_def("p2_cross", "two_defects", [("main", "a", "b"), ("other", "c", "d")],
                 cross=True, expectation="constructed: two cycles split across modules"),
        case_def("p3_same", "three_defects",
                 [("main", "a", "b"), ("main", "c", "d"), ("main", "e", "f")],
                 bounds=dict(P3_BOUNDS),
                 expectation="constructed: three independent cycles in main (bounded analysis)"),
        case_def("p3_cross", "three_defects",
                 [("main", "a", "b"), ("other", "c", "d"), ("main", "e", "f")],
                 cross=True, bounds=dict(P3_BOUNDS),
                 expectation="constructed: three cycles, mixed modules (bounded analysis)"),
        case_def("p1_interf", "interference", [("main", "a", "b")],
                 interference=[("main", "i1", "j1"), ("main", "i2", "j2")],
                 expectation="one defect plus two reachable benign lock pairs (extra candidates)"),
        case_def("p2_interf", "interference", [("main", "a", "b"), ("main", "c", "d")],
                 interference=[("main", "i1", "j1")],
                 expectation="two defects plus one reachable benign lock pair (extra candidates)"),
        case_def("p3_interf", "interference",
                 [("main", "a", "b"), ("main", "c", "d"), ("main", "e", "f")],
                 interference=[("main", "i1", "j1")],
                 bounds=dict(P3_BOUNDS),
                 expectation="three defects plus one reachable benign lock pair (bounded analysis)"),
        case_def("p2_names_swapped", "enumerate_order",
                 [("main", "a", "b"), ("main", "c", "d")], name_order=True,
                 expectation="same construction as p2_same, worker declaration order permuted"),
        case_def("p2_modules_swapped", "enumerate_order",
                 [("main", "a", "b"), ("other", "c", "d")], cross=True, module_order=True,
                 expectation="same construction as p2_cross, module order permuted"),
        case_def("c_already_correct", "controls", [],
                 interference=[("main", "a", "b")],
                 expectation="already correct: one consistently ordered lock pair, no cycle"),
        case_def("c_already_correct_interf", "controls", [],
                 interference=[("main", "i1", "j1"), ("main", "i2", "j2")],
                 expectation="already correct with benign lock traffic"),
        case_def("c_scope_restricted", "controls", [("main", "a", "b")],
                 scope_functions=["main::main"],
                 expectation="defect exists but the edit scope forbids all worker functions"),
        case_def("c_lock_reorder_forbidden", "controls", [("main", "a", "b")],
                 allow_lock_reorder=False,
                 expectation="defect exists but the contract forbids lock reordering"),
        case_def("c_bounds_unknown", "controls", [("main", "a", "b"), ("main", "c", "d")],
                 bounds=dict(UNKNOWN_BOUNDS),
                 expectation="two defects, frozen analysis bounds too small: must stay UNKNOWN"),
        case_def("c_preserved_unsatisfiable", "controls", [("main", "a", "b")],
                 unspawned=("ghost",), witness_expected="FAIL",
                 expectation="cycle is fixable by a lock swap, but a preserved goal "
                             "(ghost completes) is unreachable for every edit: no acceptable candidate"),
    ]

    manifest_cases = []
    for c in cases:
        (CASES / f"{c['case']}.json").write_text(json.dumps(c["program"], indent=2) + "\n")
        (CASES / f"{c['case']}_contract.json").write_text(
            json.dumps(
                contract(
                    c["case"],
                    c["preserved_fqns"],
                    bounds=c["bounds"],
                    functions=c["scope_functions"],
                    allow_lock_reorder=c["allow_lock_reorder"],
                ),
                indent=2,
            )
            + "\n"
        )
        (WITNESS / f"{c['case']}_witness.json").write_text(
            json.dumps(c["witness_program"], indent=2) + "\n"
        )
        # The witness is checked under the frozen contract semantics but with the
        # full analysis bound, so a deliberately-bounded pilot case (p3_*,
        # c_bounds_unknown) does not hide a valid witness behind its pilot bound.
        (WITNESS / f"{c['case']}_contract.json").write_text(
            json.dumps(
                contract(
                    c["case"] + "_witness",
                    c["preserved_fqns"],
                    bounds=dict(MAIN_BOUNDS, max_states=1_000_000),
                    functions=c["scope_functions"],
                    allow_lock_reorder=c["allow_lock_reorder"],
                ),
                indent=2,
            )
            + "\n"
        )
        manifest_cases.append(
            {
                "case": c["case"],
                "family": c["family"],
                "model": c["model"],
                "contract": c["contract"],
                "witness": c["witness"],
                "witness_contract": f"witness/{c['case']}_contract.json",
                "witness_expected": c["witness_expected"],
                "params": c["params"],
                "origin": c["origin"],
                "expected_note": c["expected_note"],
                "known_witness_note": c["known_witness_note"],
                "minimal_patch_note": c["minimal_patch_note"],
                "independent_expectation": {
                    "defect_constructed": c["params"]["defect_units"] > 0,
                    "already_correct": c["params"]["defect_units"] == 0,
                    "fix_out_of_scope": not c["allow_lock_reorder"]
                    or c["scope_functions"] == ["main::main"],
                    "bounded_analysis": c["bounds"].get("max_states", 1)
                    < MAIN_BOUNDS["max_states"],
                    "preserved_unsatisfiable": c["witness_expected"] != "PASS",
                },
            }
        )

    smoke = [
        {"case": "already_correct", "family": "smoke_dev",
         "model": "../../tests/repro_bench/already_correct.json",
         "contract": "../../tests/repro_bench/already_correct_contract.json"},
        {"case": "single_cycle", "family": "smoke_dev",
         "model": "../../tests/repro_bench/single_cycle.json",
         "contract": "../../tests/repro_bench/single_cycle_contract.json"},
        {"case": "two_cycles", "family": "smoke_dev",
         "model": "../../tests/repro_bench/two_cycles.json",
         "contract": "../../tests/repro_bench/two_cycles_contract.json"},
        {"case": "cross_module_two_cycles", "family": "smoke_dev",
         "model": "../../tests/repro_bench/cross_module_two_cycles.json",
         "contract": "../../tests/repro_bench/cross_module_two_cycles_contract.json"},
        {"case": "forbidden_scope", "family": "smoke_dev",
         "model": "../../tests/repro_bench/single_cycle.json",
         "contract": "../../tests/repro_bench/forbidden_scope_contract.json"},
        {"case": "preserved_unfixable", "family": "smoke_dev",
         "model": "../../tests/repro_bench/preserved_unfixable.json",
         "contract": "../../tests/repro_bench/preserved_unfixable_contract.json"},
        {"case": "no_lock_candidate", "family": "smoke_dev",
         "model": "../../tests/repro_bench/channel_deadlock.json",
         "contract": "../../tests/repro_bench/channel_deadlock_contract.json"},
        {"case": "budget_truncated", "family": "smoke_dev",
         "model": "../../tests/repro_bench/two_cycles.json",
         "contract": "../../tests/repro_bench/two_cycles_contract.json",
         "config_override": {"candidate_budget": 1}},
    ]

    manifest = {
        "schema": "concir-pilot-manifest-v1",
        "note": (
            "Pilot/development data generated from a documented template. Not a "
            "held-out test set; no generalisation claim."
        ),
        "generator": "experiments/pilot-v1/generate_cases.py",
        "bounds_policy": {
            "main": MAIN_BOUNDS,
            "unknown_control": UNKNOWN_BOUNDS,
            "note": "analysis bounds are frozen in each contract and shared by A/B/C",
        },
        "search_configs": {
            "main": {"candidate_budget": 64, "verification_budget": 64, "max_depth": 4, "max_total_edits": 4},
            "tight": {"candidate_budget": 8, "verification_budget": 4, "max_depth": 1, "max_total_edits": 1},
        },
        "case_count": len(manifest_cases),
        "cases": manifest_cases,
        "smoke_cases": smoke,
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {len(manifest_cases)} pilot cases + {len(smoke)} smoke cases to {ROOT}")


if __name__ == "__main__":
    main()
