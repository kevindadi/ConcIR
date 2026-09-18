#!/usr/bin/env python3
"""Generate the corrected pilot-v2 case set.

Corrections over pilot-v1 (see REVIEW.md R6):

* `cross_shared` is a *real* cross-module contention: the two functions of a
  defect unit live in different modules and lock the same two mutexes through
  `requires.resources` FQN bindings. `cross_independent` is the distinct case
  where each module has its own local deadlock.
* A legal witness is produced by an allowed adjacent `mutex_lock` swap on the
  original program (stable sids, all unlocks preserved), saved together with the
  swap list. `pilot_tool witness` re-derives the actual patch chain and
  fingerprints and verifies it against the original frozen contract.
* Witness analysis under an enlarged bound is a separate `expanded_contract`;
  the original contract is never widened in place.
* Structural assertions confirm that cross / name-order / module-order /
  interference parameters really change the intended dimension.

Run:  python3 experiments/pilot-v2/generate_cases.py
"""

from __future__ import annotations

import copy
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CASES = ROOT / "cases"
WITNESS = ROOT / "witness"
MATRIX = ROOT / "matrix_contracts"
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
MATRIX_MAX_STATES = [20000, 100000, 200000]


# ───────────────────────────── model builders ─────────────────────────────


def resource(name):
    return {"name": name, "kind": "sync", "type": "Mutex", "mode": "Sync"}


def lock(sid, r):
    return {"sid": sid, "kind": "mutex_lock", "resource": r}


def unlock(sid, r):
    return {"sid": sid, "kind": "mutex_unlock", "resource": r}


def ret(sid):
    return {"sid": sid, "kind": "return"}


def pair_body(r1, r2):
    return [lock("s1", r1), lock("s2", r2), unlock("s3", r2), unlock("s4", r1), ret("s5")]


def fn(name, body):
    return {"name": name, "kind": "normal", "form": "closure", "body": body}


class Mod:
    def __init__(self, name):
        self.name = name
        self.res = []
        self.funcs = []
        self.req_res = []
        self.req_fn = []

    def add_res(self, r):
        if r not in self.res:
            self.res.append(r)

    def add_fn(self, f):
        self.funcs.append(f)

    def to_json(self):
        return {
            "name": self.name,
            "provides": {
                "resources": list(self.res),
                "functions": [f["name"] for f in self.funcs],
            },
            "requires": {"resources": list(self.req_res), "functions": list(self.req_fn)},
            "resources": [resource(r) for r in self.res],
            "protection": [],
            "functions": self.funcs,
        }


class Builder:
    def __init__(self):
        self.mods: dict[str, Mod] = {}

    def mod(self, name):
        if name not in self.mods:
            self.mods[name] = Mod(name)
        return self.mods[name]

    def add_defect_same(self, mod, tag, r1, r2):
        m = self.mod(mod)
        m.add_res(r1)
        m.add_res(r2)
        m.add_fn(fn(f"{tag}ab", pair_body(r1, r2)))
        m.add_fn(fn(f"{tag}ba", pair_body(r2, r1)))
        return (mod, f"{tag}ba")

    def add_defect_shared(self, m1, r1, m2, r2, tag):
        a = self.mod(m1)
        b = self.mod(m2)
        a.add_res(r1)
        b.add_res(r2)
        a.add_fn(fn(f"{tag}ab", pair_body(r1, f"{m2}::{r2}")))
        b.add_fn(fn(f"{tag}ba", pair_body(r2, f"{m1}::{r1}")))
        if f"{m2}::{r2}" not in a.req_res:
            a.req_res.append(f"{m2}::{r2}")
        if f"{m1}::{r1}" not in b.req_res:
            b.req_res.append(f"{m1}::{r1}")
        return (m2, f"{tag}ba")

    def add_benign(self, mod, tag, r1, r2):
        m = self.mod(mod)
        m.add_res(r1)
        m.add_res(r2)
        m.add_fn(fn(tag, pair_body(r1, r2)))

    def add_ghost(self, name):
        self.mod("main").add_fn(fn(name, [ret("s1")]))

    def finalize(self, unspawned=(), reverse_workers=False, module_order=None):
        main = self.mod("main")
        if reverse_workers:
            for m in self.mods.values():
                m.funcs = list(reversed(m.funcs))
        spawn = []
        for name, m in self.mods.items():
            for f in m.funcs:
                if f["name"] == "main" or f["name"] in unspawned:
                    continue
                fq = f"{name}::{f['name']}" if name != "main" else f["name"]
                if name != "main":
                    main.req_fn.append(fq)
                spawn.append(fq)
        if reverse_workers:
            spawn = list(reversed(spawn))
        main.funcs.insert(0, fn("main", [{"sid": "s1", "kind": "scope", "funcs": spawn}, ret("s2")]))
        order = list(self.mods.keys())
        if module_order == "main_last":
            order = [n for n in order if n != "main"] + ["main"]
        return {
            "program": "pilot",
            "version": VERSION,
            "entry": "main::main",
            "modules": [self.mods[n].to_json() for n in order],
        }


# ───────────────────────────── witness construction ─────────────────────────────


def apply_swaps(program, swaps):
    """Swap the statements at (module, function, sidA, sidB), preserving sids."""
    out = copy.deepcopy(program)
    for sw in swaps:
        mod = next(m for m in out["modules"] if m["name"] == sw["module"])
        f = next(x for x in mod["functions"] if x["name"] == sw["function"])
        ia = next(i for i, s in enumerate(f["body"]) if s["sid"] == sw["a"])
        ib = next(i for i, s in enumerate(f["body"]) if s["sid"] == sw["b"])
        f["body"][ia], f["body"][ib] = f["body"][ib], f["body"][ia]
    return out


def defect_swaps(defect_info):
    return [{"module": m, "function": f, "a": "s1", "b": "s2"} for (m, f) in defect_info]


# ───────────────────────────── contracts ─────────────────────────────


def contract(name, preserved, bounds, *, scope_functions=None, allow_lock_reorder=True):
    scope = {"allow_lock_reorder": allow_lock_reorder}
    if scope_functions is not None:
        scope["functions"] = scope_functions
    return {
        "name": name,
        "properties": [{"kind": "deadlock_free", "id": "no-deadlock"}],
        "preserved": [
            {"kind": "reachable", "description": f"{f} completes",
             "goal": {"kind": "function_completed", "function": f}}
            for f in preserved
        ],
        "assumptions": {"sequential_consistency": True, "no_spurious_wakeups": True},
        "bounds": bounds,
        "allowed_scope": scope,
    }


# ───────────────────────────── cases ─────────────────────────────


def make_case(case_id, family, params, program, preserved, *, bounds=None,
              scope_functions=None, allow_lock_reorder=True,
              defect_info=(), swaps=None, unspawned=(), expectation="",
              allow_edit_space_fixable=True, witness_kind="legal",
              auxiliary_expected=None, structure_facts=None):
    bounds = bounds or MAIN_BOUNDS
    cid = case_id
    witness_program = apply_swaps(program, swaps) if (swaps and witness_kind == "legal") else None
    aux = apply_swaps(program, swaps) if (swaps and witness_kind == "auxiliary") else None
    return {
        "case": cid,
        "family": family,
        "params": params,
        "program": program,
        "preserved": preserved,
        "bounds": bounds,
        "scope_functions": scope_functions,
        "allow_lock_reorder": allow_lock_reorder,
        "defect_info": list(defect_info),
        "defect_units": len(defect_info),
        "swaps": swaps or [],
        "unspawned": list(unspawned),
        "expectation": expectation,
        "allow_edit_space_fixable": allow_edit_space_fixable,
        "witness_kind": witness_kind,
        "witness_program": witness_program,
        "auxiliary_program": aux,
        "auxiliary_expected": auxiliary_expected,
        "structure_facts": structure_facts or {},
        "model": f"cases/{cid}.json",
        "contract": f"cases/{cid}_contract.json",
        "witness_path": f"witness/{cid}_witness.json" if witness_program else None,
        "auxiliary_path": f"witness/{cid}_auxiliary.json" if aux else None,
    }


def case_p1_same():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b")]
    prog = b.finalize()
    return make_case(
        "p1_same", "one_defect", {"defect_units": 1, "interference_pairs": 0,
                                  "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba"], defect_info=info,
        swaps=defect_swaps(info),
        expectation="constructed: one lock-order cycle in module main",
        structure_facts={"modules_with_workers": ["main"], "cross_resources": []},
    )


def case_p1_cross_shared():
    b = Builder()
    info = [b.add_defect_shared("main", "a", "other", "b", "u1")]
    prog = b.finalize()
    return make_case(
        "p1_cross_shared", "one_defect", {"defect_units": 1, "interference_pairs": 0,
                                          "cross": "shared", "order": "base"},
        prog, ["main::u1ab", "other::u1ba"], defect_info=info,
        swaps=defect_swaps(info),
        expectation="constructed: one shared-lock deadlock whose two sides live in different modules",
        structure_facts={"modules_with_workers": ["main", "other"],
                         "cross_resources": ["main::a", "other::b"]},
    )


def case_p1_interf():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b")]
    b.add_benign("main", "i1", "i", "j")
    prog = b.finalize()
    return make_case(
        "p1_interf", "interference", {"defect_units": 1, "interference_pairs": 1,
                                      "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::i1"], defect_info=info,
        swaps=defect_swaps(info),
        expectation="one defect plus one reachable benign lock pair (extra candidate)",
        structure_facts={"modules_with_workers": ["main"], "benign_funcs": ["i1"]},
    )


def case_p2_same():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("main", "u2", "c", "d")]
    prog = b.finalize()
    return make_case(
        "p2_same", "two_defects", {"defect_units": 2, "interference_pairs": 0,
                                   "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::u2ab", "main::u2ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="constructed: two independent cycles in main",
        structure_facts={"modules_with_workers": ["main"]},
    )


def case_p2_cross_independent():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("other", "u2", "c", "d")]
    prog = b.finalize()
    return make_case(
        "p2_cross_independent", "two_defects",
        {"defect_units": 2, "interference_pairs": 0,
         "cross": "independent", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "other::u2ab", "other::u2ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="two independent local deadlocks, one per module (not shared locks)",
        structure_facts={"modules_with_workers": ["main", "other"], "cross_resources": []},
    )


def case_p2_cross_shared():
    b = Builder()
    info = [b.add_defect_shared("main", "a", "other", "b", "u1"),
            b.add_defect_shared("main", "c", "other", "d", "u2")]
    prog = b.finalize()
    return make_case(
        "p2_cross_shared", "two_defects",
        {"defect_units": 2, "interference_pairs": 0,
         "cross": "shared", "order": "base"},
        prog, ["main::u1ab", "other::u1ba", "main::u2ab", "other::u2ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="two shared-lock deadlocks, each spanning main and other",
        structure_facts={"modules_with_workers": ["main", "other"],
                         "cross_resources": ["main::a", "other::b", "main::c", "other::d"]},
    )


def case_p2_interf():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("main", "u2", "c", "d")]
    b.add_benign("main", "i1", "i", "j")
    prog = b.finalize()
    return make_case(
        "p2_interf", "interference", {"defect_units": 2, "interference_pairs": 1,
                                      "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::u2ab", "main::u2ba", "main::i1"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="two defects plus one reachable benign lock pair",
        structure_facts={"modules_with_workers": ["main"], "benign_funcs": ["i1"]},
    )


def case_p3_same():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("main", "u2", "c", "d"),
            b.add_defect_same("main", "u3", "e", "f")]
    prog = b.finalize()
    return make_case(
        "p3_same", "three_defects", {"defect_units": 3, "interference_pairs": 0,
                                     "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::u2ab", "main::u2ba",
               "main::u3ab", "main::u3ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="constructed: three independent cycles in main (unified 200k bound)",
        structure_facts={"modules_with_workers": ["main"]},
    )


def case_p3_cross_independent():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("other", "u2", "c", "d"),
            b.add_defect_same("other", "u3", "e", "f")]
    prog = b.finalize()
    return make_case(
        "p3_cross_independent", "three_defects",
        {"defect_units": 3, "interference_pairs": 0,
         "cross": "independent", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "other::u2ab", "other::u2ba",
               "other::u3ab", "other::u3ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="three independent local deadlocks across two modules",
        structure_facts={"modules_with_workers": ["main", "other"], "cross_resources": []},
    )


def case_c_already_correct():
    b = Builder()
    b.add_benign("main", "i1", "a", "b")
    prog = b.finalize()
    return make_case(
        "c_already_correct", "controls", {"defect_units": 0, "interference_pairs": 1,
                                          "cross": "none", "order": "base"},
        prog, ["main::i1"], defect_info=[], swaps=[],
        expectation="already correct: one consistently ordered lock pair",
        allow_edit_space_fixable=None, witness_kind="none",
        structure_facts={"modules_with_workers": ["main"]},
    )


def case_c_scope_restricted():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b")]
    prog = b.finalize()
    return make_case(
        "c_scope_restricted", "controls", {"defect_units": 1, "interference_pairs": 0,
                                           "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba"], defect_info=info,
        swaps=defect_swaps(info), scope_functions=["main::main"],
        expectation="defect exists but the edit scope forbids every worker function",
        allow_edit_space_fixable=False, witness_kind="auxiliary", auxiliary_expected="PASS",
        structure_facts={"modules_with_workers": ["main"], "editable_workers": []},
    )


def case_c_lock_reorder_forbidden():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b")]
    prog = b.finalize()
    return make_case(
        "c_lock_reorder_forbidden", "controls", {"defect_units": 1, "interference_pairs": 0,
                                                 "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba"], defect_info=info,
        swaps=defect_swaps(info), allow_lock_reorder=False,
        expectation="defect exists but the contract forbids lock reordering",
        allow_edit_space_fixable=False, witness_kind="auxiliary", auxiliary_expected="PASS",
        structure_facts={"modules_with_workers": ["main"], "editable_workers": []},
    )


def case_c_preserved_unsatisfiable():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b")]
    b.add_ghost("ghost")
    prog = b.finalize(unspawned=("ghost",))
    return make_case(
        "c_preserved_unsatisfiable", "controls",
        {"defect_units": 1, "interference_pairs": 0, "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::ghost"], defect_info=info,
        swaps=defect_swaps(info), unspawned=("ghost",),
        expectation="cycle is fixable by a swap, but preserved 'ghost completes' is unreachable",
        allow_edit_space_fixable=False, witness_kind="none",
        structure_facts={"modules_with_workers": ["main"], "unspawned": ["ghost"]},
    )


def case_c_bounds_unknown():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("main", "u2", "c", "d")]
    prog = b.finalize()
    return make_case(
        "c_bounds_unknown", "controls", {"defect_units": 2, "interference_pairs": 0,
                                         "cross": "none", "order": "base"},
        prog, ["main::u1ab", "main::u1ba", "main::u2ab", "main::u2ba"],
        bounds=dict(UNKNOWN_BOUNDS), defect_info=info, swaps=defect_swaps(info),
        expectation="two defects but the frozen analysis bound is too small: must stay UNKNOWN",
        allow_edit_space_fixable=None, witness_kind="legal",
        structure_facts={"modules_with_workers": ["main"]},
    )


def case_e_name_order():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("main", "u2", "c", "d")]
    prog = b.finalize(reverse_workers=True)
    return make_case(
        "e_name_order", "enumerate_order", {"defect_units": 2, "interference_pairs": 0,
                                            "cross": "none", "order": "workers_reversed"},
        prog, ["main::u1ab", "main::u1ba", "main::u2ab", "main::u2ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="same construction as p2_same with worker declaration/order reversed",
        structure_facts={"modules_with_workers": ["main"], "order": "workers_reversed"},
    )


def case_e_module_order():
    b = Builder()
    info = [b.add_defect_same("main", "u1", "a", "b"),
            b.add_defect_same("other", "u2", "c", "d")]
    prog = b.finalize(module_order="main_last")
    return make_case(
        "e_module_order", "enumerate_order",
        {"defect_units": 2, "interference_pairs": 0,
         "cross": "independent", "order": "modules_main_last"},
        prog, ["main::u1ab", "main::u1ba", "other::u2ab", "other::u2ba"],
        defect_info=info, swaps=defect_swaps(info),
        expectation="same construction as p2_cross_independent with the module order permuted",
        structure_facts={"modules_with_workers": ["main", "other"],
                         "module_order_starts_with": "other"},
    )


CASES_FUNCS = [
    case_p1_same, case_p1_cross_shared, case_p1_interf,
    case_p2_same, case_p2_cross_independent, case_p2_cross_shared, case_p2_interf,
    case_p3_same, case_p3_cross_independent,
    case_c_already_correct, case_c_scope_restricted, case_c_lock_reorder_forbidden,
    case_c_preserved_unsatisfiable, case_c_bounds_unknown,
    case_e_name_order, case_e_module_order,
]


# ───────────────────────────── structural assertions ─────────────────────────────


def worker_names(program):
    names = []
    for m in program["modules"]:
        for f in m["functions"]:
            if f["name"] != "main":
                names.append((m["name"], f["name"]))
    return names


def assert_structure(case):
    p = case["program"]
    facts = case["structure_facts"]
    mods_with_workers = sorted({m["name"] for m in p["modules"]
                                if any(f["name"] != "main" for f in m["functions"])})
    assert mods_with_workers == sorted(facts.get("modules_with_workers", [])), \
        f"{case['case']}: worker modules {mods_with_workers} != {facts.get('modules_with_workers')}"
    cross = sorted({s["resource"] for m in p["modules"] for f in m["functions"]
                    for s in f["body"] if s.get("kind") == "mutex_lock"
                    and "::" in s["resource"]})
    assert cross == sorted(facts.get("cross_resources", [])), \
        f"{case['case']}: cross resources {cross} != {facts.get('cross_resources')}"
    # cross_shared must list the foreign resource in requires.resources
    for m in p["modules"]:
        for s in [st for f in m["functions"] for st in f["body"] if st.get("kind") == "mutex_lock"]:
            if "::" in s["resource"]:
                assert s["resource"] in m["requires"]["resources"], \
                    f"{case['case']}: {m['name']} uses {s['resource']} without requires.resources"
    # ba functions == defect units
    ba = [1 for m in p["modules"] for f in m["functions"] if f["name"].endswith("ba")]
    assert len(ba) == case["defect_units"], f"{case['case']}: defect units mismatch"
    # witness swaps must name the ba lock pair
    for sw in case["swaps"]:
        mod = next(m for m in p["modules"] if m["name"] == sw["module"])
        f = next(f for f in mod["functions"] if f["name"] == sw["function"])
        body = {s["sid"]: s for s in f["body"]}
        assert body[sw["a"]]["kind"] == "mutex_lock" and body[sw["b"]]["kind"] == "mutex_lock", \
            f"{case['case']}: swap targets are not mutex locks"
        assert body[sw["a"]]["resource"] != body[sw["b"]]["resource"], \
            f"{case['case']}: swap targets the same resource"
    if facts.get("benign_funcs"):
        names = {f["name"] for m in p["modules"] for f in m["functions"]}
        for bn in facts["benign_funcs"]:
            assert bn in names, f"{case['case']}: benign function {bn} missing"
    if facts.get("unspawned"):
        scoped = {ref for m in p["modules"] for f in m["functions"] if f["name"] == "main"
                  for st in f["body"] if st.get("kind") == "scope" for ref in st["funcs"]}
        for g in facts["unspawned"]:
            assert g not in scoped and f"main::{g}" not in scoped, \
                f"{case['case']}: ghost {g} must not be spawned"
    if facts.get("module_order_starts_with"):
        assert p["modules"][0]["name"] == facts["module_order_starts_with"], \
            f"{case['case']}: module order not permuted"
    if case["params"].get("order") == "workers_reversed":
        base = [n for n in worker_names(case_p2_same()["program"])]
        assert worker_names(p) == list(reversed(base)), \
            f"{case['case']}: worker order not reversed vs p2_same"
    # cross_shared vs cross_independent coverage distinction
    if case["params"].get("cross") == "shared":
        assert mods_with_workers and len(mods_with_workers) >= 2 and cross, "cross_shared not shared"
    if case["params"].get("cross") == "independent":
        assert len(mods_with_workers) >= 2 and not cross, "cross_independent shares resources"
    if case["params"].get("cross") == "none":
        assert not cross and mods_with_workers in ([], ["main"]), "same-module case leaks modules"


# ───────────────────────────── main ─────────────────────────────


def main():
    for d in (CASES, WITNESS, MATRIX):
        d.mkdir(parents=True, exist_ok=True)

    cases = [f() for f in CASES_FUNCS]
    # Heavy (3-defect) cases run last so a per-process timeout cannot starve the
    # cheap cases if the batch budget is hit.
    cases.sort(key=lambda c: 1 if c["defect_units"] >= 3 else 0)
    for c in cases:
        assert_structure(c)

    manifest_cases = []
    same_witness_none = []
    for c in cases:
        cid = c["case"]
        (CASES / f"{cid}.json").write_text(json.dumps(c["program"], indent=2) + "\n")
        (CASES / f"{cid}_contract.json").write_text(json.dumps(
            contract(cid, c["preserved"], c["bounds"],
                     scope_functions=c["scope_functions"],
                     allow_lock_reorder=c["allow_lock_reorder"]), indent=2) + "\n")
        witness_entry = {"kind": c["witness_kind"], "swaps": c["swaps"]}
        if c["witness_program"] is not None:
            (WITNESS / f"{cid}_witness.json").write_text(
                json.dumps(c["witness_program"], indent=2) + "\n")
            # expanded-bound contract for cases whose frozen bound may make the
            # witness UNKNOWN; labelled separately, never replacing the original.
            expanded = contract(cid + "_witness_expanded", c["preserved"],
                                dict(MAIN_BOUNDS, max_states=1000000),
                                scope_functions=None, allow_lock_reorder=True)
            (WITNESS / f"{cid}_witness_expanded_contract.json").write_text(
                json.dumps(expanded, indent=2) + "\n")
            witness_entry["expanded_contract"] = f"witness/{cid}_witness_expanded_contract.json"
        if c["auxiliary_program"] is not None:
            (WITNESS / f"{cid}_auxiliary.json").write_text(
                json.dumps(c["auxiliary_program"], indent=2) + "\n")
            witness_entry["auxiliary_program"] = f"witness/{cid}_auxiliary.json"
            witness_entry["expected"] = c["auxiliary_expected"]
        witness_entry["expected"] = witness_entry.get("expected") or (
            "PASS" if c["witness_kind"] == "legal" else
            ("PASS" if cid == "c_already_correct" else "FAIL"))
        if c["witness_kind"] == "legal" and not c["defect_units"]:
            same_witness_none.append(cid)

        k = c["defect_units"]
        manifest_cases.append({
            "case": cid,
            "family": c["family"],
            "model": c["model"],
            "contract": c["contract"],
            "witness": witness_entry,
            "params": c["params"],
            "defect_units": k,
            "edit_lower_bound": k if k else 0,
            "edit_lower_bound_basis": (
                "argument: k disjoint lock pairs, each needs >= 1 adjacent lock swap"
                if k else "no constructed defect"),
            "allowed_edit_space_fixable": c["allow_edit_space_fixable"],
            "expected_witness_len": k if c["witness_kind"] == "legal" and k else None,
            "minimal_patch_len": (
                k if (c["witness_kind"] == "legal" and k
                      and c["allow_edit_space_fixable"] is True) else None),
            "minimal_patch_basis": (
                "disjoint-pair lower bound matched by the verified witness upper bound"
                if (c["witness_kind"] == "legal" and k
                    and c["allow_edit_space_fixable"] is True)
                else "not established for this control"),
            "expectation": c["expectation"],
            "structure_facts": c["structure_facts"],
            "repeat_policy": ({"main": 1, "tight": 1} if k >= 3 else {"main": 3, "tight": 1}),
            "independent_expectation": {
                "defect_constructed": k > 0,
                "already_correct": k == 0,
                "fix_out_of_scope": c["witness_kind"] == "auxiliary",
                "preserved_unsatisfiable": cid == "c_preserved_unsatisfiable",
                "bounded_analysis": c["bounds"]["max_states"] < MAIN_BOUNDS["max_states"],
            },
        })

    # budget matrix contracts for 4 representative cases
    matrix_cases = []
    for cid in ["p1_same", "p2_same", "p3_same", "p1_interf"]:
        c = next(x for x in cases if x["case"] == cid)
        variants = []
        for ms in MATRIX_MAX_STATES:
            bounds = dict(MAIN_BOUNDS, max_states=ms)
            label = f"ms{ms}"
            (MATRIX / f"{cid}_{label}_contract.json").write_text(json.dumps(
                contract(f"{cid}-{label}", c["preserved"], bounds,
                         scope_functions=c["scope_functions"],
                         allow_lock_reorder=c["allow_lock_reorder"]), indent=2) + "\n")
            variants.append({"label": label, "contract": f"matrix_contracts/{cid}_{label}_contract.json",
                             "engine": "petri", "max_states": ms})
        matrix_cases.append({"case": cid, "model": c["model"], "family": c["family"],
                             "params": c["params"], "variants": variants})

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
        "schema": "concir-pilot-manifest-v2",
        "note": "Generated development pre-experiment; not a held-out test set.",
        "generator": "experiments/pilot-v2/generate_cases.py",
        "bounds_policy": {
            "unified_main": MAIN_BOUNDS,
            "unknown_control": UNKNOWN_BOUNDS,
            "matrix_max_states": MATRIX_MAX_STATES,
            "note": "Pilot cases share max_states=200000; three-defect cases are no "
                    "longer given a smaller analysis bound. Root sensitivity is measured "
                    "by the separate matrix batch.",
        },
        "search_configs": {
            "main": {"candidate_budget": 64, "verification_budget": 64,
                     "max_depth": 4, "max_total_edits": 4},
            "tight": {"candidate_budget": 8, "verification_budget": 4,
                      "max_depth": 1, "max_total_edits": 1},
        },
        "case_count": len(manifest_cases),
        "cases": manifest_cases,
        "matrix_cases": matrix_cases,
        "smoke_cases": smoke,
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {len(manifest_cases)} pilot cases, {len(matrix_cases)} matrix cases, "
          f"{len(smoke)} smoke cases to {ROOT}")


if __name__ == "__main__":
    main()
