//! Code-to-Model Branch-Pruning Stuttering Refinement Checker
//! (`concir-backend verify-refinement`, schema `concir-refinement-report/1`).
//!
//! Verifies $N_{\text{impl}} \preceq_{\text{ref}} N_{\text{spec}}$:
//! 1. `O1_purity` (`hole_purity`): zero `purity_violations` in `impl_sync_net.json`.
//! 2. `O2_weak_simulation` (`weak_simulation`): greatest fixed-point weak simulation
//!    over synchronization resource primitives $\Sigma_{\text{sync}}$ with shortest
//!    distinguishing witness trace on failure.
//! 3. `O2_deadlock_and_divergence` (`deadlock_free`, `lock_divergence_free`):
//!    no reachable deadlock state in $N_{\text{impl}}$ and no reachable $\tau$-cycle
//!    while holding locks (unless already present in spec).
//! 4. `O2_preserved_goals` (`preserved_goals`): every `function_completed` in
//!    `contract.preserved` appears in `impl_sync_net.completed_functions`, and
//!    non-triviality (if every spec run performs a sync op, impl must too).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use unipn::analysis::refinement::check_stuttering_refinement;

use crate::ast::Program;
use crate::codegen::CodegenMap;
use crate::explore::contract::{ContractSpec, PredicateSpec, PreservedSpec};
use crate::explore::explore;
use crate::fqn;
use crate::petri::net::{MutexToken, NetState, PlaceKey, RwLockToken};
use crate::petri::PetriEngine;
use crate::sem::ids::ResourceId;
use crate::sem::outcome::{AnalysisBounds, Phase};
use crate::sem::program::{self, ResKind, SemOp, SemProgram};
use crate::sem::system::TransitionSystem;

pub const IMPL_NET_SCHEMA: &str = "concir-impl-net/1";
pub const REFINEMENT_REPORT_SCHEMA: &str = "concir-refinement-report/1";

// ──────────────────── Input Schema (`concir-impl-net/1`) ────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplPlace {
    pub id: usize,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub resource_kind: Option<String>,
    #[serde(default)]
    pub initial_tokens: usize,
    #[serde(default)]
    pub span: Option<String>,
}

fn default_arc_weight() -> usize {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ImplArc {
    Place(usize),
    Weighted {
        place: usize,
        #[serde(default = "default_arc_weight")]
        weight: usize,
    },
}

impl ImplArc {
    pub fn place(self) -> usize {
        match self {
            ImplArc::Place(p) => p,
            ImplArc::Weighted { place, .. } => place,
        }
    }

    pub fn weight(self) -> usize {
        match self {
            ImplArc::Place(_) => 1,
            ImplArc::Weighted { weight, .. } => weight,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplTransition {
    pub id: usize,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub observable: bool,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub fn_def_path: Option<String>,
    #[serde(default)]
    pub bb: Option<usize>,
    #[serde(default)]
    pub span: Option<String>,
    #[serde(default)]
    pub hole_id: Option<String>,
    #[serde(default)]
    pub inputs: Vec<ImplArc>,
    #[serde(default)]
    pub outputs: Vec<ImplArc>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplState {
    pub id: usize,
    #[serde(default)]
    pub initial: bool,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub deadlock: bool,
    #[serde(default)]
    pub held_locks: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplEdge {
    pub from: usize,
    pub to: usize,
    pub transition: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplPurityViolation {
    #[serde(default)]
    pub hole_id: Option<String>,
    #[serde(default)]
    pub fn_def_path: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub span: Option<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ImplNet {
    #[serde(default)]
    pub schema: String,
    #[serde(rename = "crate", default)]
    pub crate_name: String,
    #[serde(default)]
    pub entry_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_module: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub places: Vec<ImplPlace>,
    #[serde(default)]
    pub transitions: Vec<ImplTransition>,
    #[serde(default)]
    pub states: Vec<ImplState>,
    #[serde(default)]
    pub edges: Vec<ImplEdge>,
    #[serde(default)]
    pub purity_violations: Vec<ImplPurityViolation>,
    #[serde(default)]
    pub completed_functions: Vec<String>,
}

// ──────────────────── Output Schema (`concir-refinement-report/1`) ────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementChecks {
    pub hole_purity: bool,
    pub weak_simulation: bool,
    pub deadlock_free: bool,
    pub lock_divergence_free: bool,
    pub preserved_goals: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessStep {
    pub step: usize,
    pub op: String,
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementViolation {
    pub obligation: String,
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hole_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default)]
    pub req_clauses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust_span: Option<String>,
    #[serde(default)]
    pub witness_trace: Vec<WitnessStep>,
    #[serde(default)]
    pub expected_spec_ops: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementReport {
    pub schema: String,
    pub outcome: String,
    pub relation: String,
    pub checks: RefinementChecks,
    pub spec_states: usize,
    pub impl_states: usize,
    pub violations: Vec<RefinementViolation>,
}

// ──────────────────── Core Algorithm ────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SyncAction {
    op: String,
    resource: String,
}

impl SyncAction {
    fn display(&self) -> String {
        if self.resource.is_empty() {
            self.op.clone()
        } else {
            format!("{}({})", self.op, self.resource)
        }
    }
}

/// Synchronization resource primitives forming $\Sigma_{\text{sync}}$.
/// Thread lifecycle ops (`spawn`, `join`, `scope`) are projected to $\tau$
/// so `std::thread::scope` vs `JoinHandle` is representation-independent, while
/// thread completion is verified via `preserved_goals`.
pub fn is_sync_resource_op(op: &str) -> bool {
    matches!(
        op,
        "mutex_lock"
            | "mutex_unlock"
            | "rwlock_read"
            | "rwlock_write"
            | "rwlock_unlock"
            | "rwlock_read_lock"
            | "rwlock_read_unlock"
            | "rwlock_write_lock"
            | "rwlock_write_unlock"
            | "condvar_wait"
            | "condvar_notify"
            | "condvar_notify_all"
            | "sem_acquire"
            | "sem_release"
            | "channel_send"
            | "channel_recv"
            | "atomic_load"
            | "atomic_store"
            | "atomic_cas"
    )
}

fn normalize_op_for_matching(
    op: &str,
    collapse_notify_all: bool,
    collapse_rwlock_unlock: bool,
) -> String {
    match op {
        "rwlock_read" | "rwlock_read_lock" => "rwlock_read_lock".to_string(),
        "rwlock_write" | "rwlock_write_lock" => "rwlock_write_lock".to_string(),
        "rwlock_read_unlock" | "rwlock_write_unlock" | "rwlock_unlock"
            if collapse_rwlock_unlock =>
        {
            "rwlock_unlock".to_string()
        }
        "condvar_notify_all" if collapse_notify_all => "condvar_notify".to_string(),
        other => other.to_string(),
    }
}

struct ModuleScope {
    mod_name: String,
    mod_id: crate::sem::ids::ModuleId,
    sanitized_mod: String,
    in_scope_resources: HashSet<String>,
    in_scope_fn_short: HashSet<String>,
}

impl ModuleScope {
    fn from_program(program: &Program, sem: &SemProgram, mod_name: &str) -> Result<Self, String> {
        let Some(ast_mod) = program.lookup_module(mod_name) else {
            return Err(format!("module '{mod_name}' not found in program"));
        };
        let Some(mod_id) = sem.module_id(mod_name) else {
            return Err(format!("module '{mod_name}' not found in lowered program"));
        };
        let mut in_scope_resources = HashSet::new();
        for r in &ast_mod.resources {
            in_scope_resources.insert(fqn::fqn(&ast_mod.name, &r.name));
        }
        let in_scope_fn_short: HashSet<String> = ast_mod
            .functions
            .iter()
            .map(|f| f.name.clone())
            .collect();
        Ok(ModuleScope {
            mod_name: mod_name.to_string(),
            mod_id,
            sanitized_mod: sanitize_ident(mod_name),
            in_scope_resources,
            in_scope_fn_short,
        })
    }

    fn fn_path_in_module(&self, fn_def_path: Option<&str>) -> bool {
        let Some(path) = fn_def_path else {
            return false;
        };
        let seg_mod = format!("::{}::", self.sanitized_mod);
        let seg_raw = format!("::{}::", self.mod_name);
        let prefix_mod = format!("{}::", self.sanitized_mod);
        let prefix_raw = format!("{}::", self.mod_name);
        path.contains(&seg_mod)
            || path.contains(&seg_raw)
            || path.starts_with(&prefix_mod)
            || path.starts_with(&prefix_raw)
    }

    fn impl_edge_in_scope(&self, resource_fqn: &str, fn_def_path: Option<&str>) -> bool {
        if self.in_scope_resources.contains(resource_fqn) {
            return true;
        }
        // Keep unauthorized/unknown sync operations inside this module visible!
        self.fn_path_in_module(fn_def_path)
    }

    fn spec_edge_in_scope(
        &self,
        resource_fqn: &str,
        origin_mod: crate::sem::ids::ModuleId,
    ) -> bool {
        self.in_scope_resources.contains(resource_fqn) || origin_mod == self.mod_id
    }

    fn fn_in_scope(&self, func_name: &str) -> bool {
        match fqn::split_fqn(func_name) {
            Some((m, e)) => m == self.mod_name && self.in_scope_fn_short.contains(e),
            None => self.in_scope_fn_short.contains(func_name),
        }
    }

    fn purity_violation_in_scope(
        &self,
        pv: &ImplPurityViolation,
        codegen_map: Option<&CodegenMap>,
    ) -> bool {
        if let (Some(hid), Some(cm)) = (pv.hole_id.as_deref(), codegen_map) {
            if let Some(h) = cm.holes.iter().find(|h| h.id == hid) {
                return h.module == self.mod_name;
            }
            if let Some(th) = cm.type_holes.iter().find(|th| th.id == hid) {
                return th.module == self.mod_name;
            }
        }
        if pv.fn_def_path.is_some() {
            return self.fn_path_in_module(pv.fn_def_path.as_deref());
        }
        true
    }
}

#[derive(Clone)]
struct EdgeMeta {
    span: Option<String>,
    hole_id: Option<String>,
}

struct Lts {
    num_states: usize,
    initial: usize,
    tau_edges: Vec<Vec<(usize, EdgeMeta)>>,
    obs_edges: Vec<Vec<(SyncAction, usize, EdgeMeta)>>,
    deadlock: Vec<bool>,
    terminal: Vec<bool>,
    holds_lock: Vec<bool>,
}

/// Run branch-pruning stuttering refinement checking across the whole program.
pub fn verify_refinement(
    program: &Program,
    impl_net: &ImplNet,
    contract: Option<&ContractSpec>,
    codegen_map: Option<&CodegenMap>,
) -> Result<RefinementReport, String> {
    verify_refinement_for_module(program, impl_net, contract, codegen_map, None)
}

/// Run branch-pruning stuttering refinement checking, optionally projected to a
/// single module (`--module <MOD>`) combined with `summarize_program` lock-order
/// cycle freedom verification.
pub fn verify_refinement_for_module(
    program: &Program,
    impl_net: &ImplNet,
    contract: Option<&ContractSpec>,
    codegen_map: Option<&CodegenMap>,
    module_filter: Option<&str>,
) -> Result<RefinementReport, String> {
    let owned;
    let program = if program.has_structured_bodies() {
        let mut lowered = program.clone();
        lowered.ensure_lowered()?;
        owned = lowered;
        &owned
    } else {
        program
    };

    let static_report = crate::validate::validate(program);
    if !static_report.valid {
        let first = static_report
            .diagnostics
            .iter()
            .find(|d| d.severity == crate::diagnostic::Severity::Error)
            .map(|d| format!("[{}] {}", d.code, d.message))
            .unwrap_or_else(|| "invalid CIR program".into());
        return Err(first);
    }

    let sem = program::lower(program).map_err(|e| e.to_string())?;
    let module_scope = match module_filter {
        Some(m) => Some(ModuleScope::from_program(program, &sem, m)?),
        None => None,
    };

    let bounds = contract
        .map(|c| AnalysisBounds::from(&c.bounds))
        .unwrap_or_default();

    // 1. Explore specification Petri net.
    let engine = PetriEngine::new(&sem, bounds.clone());
    let spec_reach = explore(&engine, &bounds);
    let spec_lts = build_spec_lts(&sem, &engine, &spec_reach, module_scope.as_ref());

    // 2. Build (or explore) implementation LTS with canonicalized sync labels.
    let canon = ResourceCanonicalizer::new(&sem, codegen_map);
    let (impl_lts, impl_truncated) = build_impl_lts(impl_net, &canon, module_scope.as_ref());
    let any_truncated = impl_net.truncated || impl_truncated || spec_reach.truncated;

    let mut violations: Vec<RefinementViolation> = Vec::new();

    // ── Check 1: O1 Hole Purity ──
    let mut relevant_purity_violations = 0usize;
    for pv in &impl_net.purity_violations {
        if let Some(scope) = module_scope.as_ref() {
            if !scope.purity_violation_in_scope(pv, codegen_map) {
                continue;
            }
        }
        relevant_purity_violations += 1;
        let (sid, req_clauses) = hole_metadata(pv.hole_id.as_deref(), codegen_map);
        violations.push(RefinementViolation {
            obligation: "O1_purity".to_string(),
            kind: "sync_in_hole".to_string(),
            message: if pv.message.is_empty() {
                format!(
                    "synchronization or forbidden construct '{}' found inside hole '{}'",
                    pv.op.as_deref().unwrap_or("unknown"),
                    pv.hole_id.as_deref().unwrap_or("?")
                )
            } else {
                pv.message.clone()
            },
            hole_id: pv.hole_id.clone(),
            sid,
            req_clauses,
            rust_span: pv.span.clone(),
            witness_trace: Vec::new(),
            expected_spec_ops: Vec::new(),
        });
    }
    let hole_purity = relevant_purity_violations == 0;

    // ── Checks 2 & 3: Delegate weak simulation, deadlock, and lock-divergence to UniPN ──
    let spec_has_lock_tau_cycle = has_lock_holding_tau_cycle(&spec_lts).is_some();
    let (weak_sim_violation, deadlock_violation, lock_div_violation) =
        run_unipn_refinement(&impl_lts, &spec_lts, spec_has_lock_tau_cycle, codegen_map);

    let weak_simulation = weak_sim_violation.is_none();
    if let Some(v) = weak_sim_violation {
        violations.push(v);
    }

    let mut deadlock_free = deadlock_violation.is_none();
    if let Some(v) = deadlock_violation {
        violations.push(v);
    }

    let lock_divergence_free = lock_div_violation.is_none();
    if let Some(v) = lock_div_violation {
        violations.push(v);
    }

    // ── Compositional lock-order cycle check when `--module <MOD>` is specified ──
    if let Some(mod_name) = module_filter {
        let summary = crate::summarize::summarize_program(program, Some(mod_name))?;
        let mod_in_cycle = summary
            .modules
            .first()
            .map(|m| m.cross_module_lock_cycle)
            .unwrap_or(false)
            || summary.cross_module_lock_cycle;
        if mod_in_cycle {
            deadlock_free = false;
            let cycles_desc: Vec<String> = summary
                .lock_cycles
                .iter()
                .map(|c| format!("[{}]", c.join(" -> ")))
                .collect();
            violations.push(RefinementViolation {
                obligation: "O2_refinement".to_string(),
                kind: "cross_module_lock_cycle".to_string(),
                message: format!(
                    "module '{mod_name}' participates in a cross-module lock-order cycle: {}",
                    cycles_desc.join(", ")
                ),
                hole_id: None,
                sid: None,
                req_clauses: Vec::new(),
                rust_span: None,
                witness_trace: Vec::new(),
                expected_spec_ops: Vec::new(),
            });
        }
    }

    // ── Check 4: Preserved Goals & Non-Triviality ──
    let preserved_violations =
        check_preserved_goals(&impl_lts, &spec_lts, impl_net, contract, module_scope.as_ref());
    let preserved_goals = preserved_violations.is_empty();
    violations.extend(preserved_violations);

    let all_pass = hole_purity
        && weak_simulation
        && deadlock_free
        && lock_divergence_free
        && preserved_goals;

    let outcome = if !all_pass {
        "fail"
    } else if any_truncated {
        "unknown"
    } else {
        "pass"
    };

    Ok(RefinementReport {
        schema: REFINEMENT_REPORT_SCHEMA.to_string(),
        outcome: outcome.to_string(),
        relation: "stuttering_refinement".to_string(),
        checks: RefinementChecks {
            hole_purity,
            weak_simulation,
            deadlock_free,
            lock_divergence_free,
            preserved_goals,
        },
        spec_states: spec_lts.num_states,
        impl_states: impl_lts.num_states,
        violations,
    })
}

fn hole_metadata(
    hole_id: Option<&str>,
    codegen_map: Option<&CodegenMap>,
) -> (Option<String>, Vec<String>) {
    let (Some(hid), Some(cm)) = (hole_id, codegen_map) else {
        return (None, Vec::new());
    };
    if let Some(h) = cm.holes.iter().find(|h| h.id == hid) {
        let sid = if h.sid.is_empty() {
            None
        } else {
            Some(h.sid.clone())
        };
        return (sid, h.req.clone());
    }
    if let Some(th) = cm.type_holes.iter().find(|th| th.id == hid) {
        return (None, th.req.clone());
    }
    (None, Vec::new())
}

// ──────────────────── Resource Name Canonicalizer ────────────────────

struct ResourceCanonicalizer {
    spec_fqns: Vec<String>,
    field_to_fqn: Vec<(String, String)>,
}

impl ResourceCanonicalizer {
    fn new(sem: &SemProgram, codegen_map: Option<&CodegenMap>) -> Self {
        let spec_fqns: Vec<String> = sem
            .resources()
            .iter()
            .map(|r| fqn::fqn(sem.module_name(r.module), &r.name))
            .collect();

        let mut field_to_fqn = Vec::new();
        if let Some(cm) = codegen_map {
            for r in &cm.resources {
                if !r.field.is_empty() && !r.fqn.is_empty() {
                    field_to_fqn.push((r.field.clone(), r.fqn.clone()));
                }
            }
        }
        // Also synthesize ConcIR default codegen field names `r_<mod>__<name>`
        // and `r<id>_<mod>_<name>` so matching works even without `--codegen-map`.
        for r in sem.resources() {
            let m = sem.module_name(r.module);
            let fqn_str = fqn::fqn(m, &r.name);
            let s_mod = sanitize_ident(m);
            let s_name = sanitize_ident(&r.name);
            field_to_fqn.push((format!("r_{s_mod}__{s_name}"), fqn_str.clone()));
            field_to_fqn.push((format!("r{}_{s_mod}_{s_name}", r.id.0), fqn_str));
        }

        // Prefer longer field names first to avoid prefix collisions.
        field_to_fqn.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

        ResourceCanonicalizer {
            spec_fqns,
            field_to_fqn,
        }
    }

    fn canonicalize(&self, raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        // 1. Exact spec FQN match.
        if self.spec_fqns.iter().any(|f| f == trimmed) {
            return trimmed.to_string();
        }
        // 2. Codegen field name match (exact or substring like `shared.r_main__m`).
        for (field, fqn_str) in &self.field_to_fqn {
            if trimmed == field
                || trimmed.ends_with(&format!(".{field}"))
                || trimmed.ends_with(&format!("::{field}"))
                || trimmed.contains(field)
            {
                return fqn_str.clone();
            }
        }
        // 3. Suffix match on short resource name (unambiguous).
        let short_matches: Vec<&String> = self
            .spec_fqns
            .iter()
            .filter(|f| {
                let short = f.rsplit("::").next().unwrap_or(f.as_str());
                trimmed == short
                    || trimmed.ends_with(&format!(".{short}"))
                    || trimmed.ends_with(&format!("::{short}"))
                    || trimmed.ends_with(&format!("__{short}"))
            })
            .collect();
        if short_matches.len() == 1 {
            return short_matches[0].clone();
        }
        // 4. Fallback: preserve raw name so unauthorized resources remain distinct.
        trimmed.to_string()
    }
}

fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// ──────────────────── Spec LTS Construction ────────────────────

fn build_spec_lts(
    sem: &SemProgram,
    engine: &PetriEngine,
    reach: &crate::explore::Reachability<NetState>,
    module_scope: Option<&ModuleScope>,
) -> Lts {
    let n = reach.states.len();
    let mut tau_edges: Vec<Vec<(usize, EdgeMeta)>> = vec![Vec::new(); n];
    let mut obs_edges: Vec<Vec<(SyncAction, usize, EdgeMeta)>> = vec![Vec::new(); n];
    let mut deadlock = vec![false; n];
    let mut terminal = vec![false; n];
    let mut holds_lock = vec![false; n];

    let empty_meta = EdgeMeta {
        span: None,
        hole_id: None,
    };

    for (u, state) in reach.states.iter().enumerate() {
        holds_lock[u] = spec_state_holds_lock(sem, engine, state);
        if reach.edges[u].is_empty() {
            if engine.is_finished(state) {
                terminal[u] = true;
            } else {
                deadlock[u] = true;
            }
        }
        for (v, label) in &reach.edges[u] {
            let dst_state = &reach.states[*v];
            if let Some(action) = classify_spec_edge(sem, engine, state, dst_state, label) {
                let keep = module_scope.map_or(true, |sc| {
                    sc.spec_edge_in_scope(&action.resource, label.origin.module)
                });
                if keep {
                    obs_edges[u].push((action, *v, empty_meta.clone()));
                } else {
                    tau_edges[u].push((*v, empty_meta.clone()));
                }
            } else {
                tau_edges[u].push((*v, empty_meta.clone()));
            }
        }
    }

    Lts {
        num_states: n,
        initial: 0,
        tau_edges,
        obs_edges,
        deadlock,
        terminal,
        holds_lock,
    }
}

fn spec_resource_fqn(sem: &SemProgram, r: ResourceId) -> String {
    let res = sem.resource(r);
    fqn::fqn(sem.module_name(res.module), &res.name)
}

fn spec_state_holds_lock(sem: &SemProgram, engine: &PetriEngine, state: &NetState) -> bool {
    for r in sem.resources() {
        if r.kind == ResKind::Mutex {
            if let Some(&pid) = engine.net.place_of.get(&PlaceKey::Mutex(r.id)) {
                if matches!(state.read_mutex(pid), Some(MutexToken::Held(_))) {
                    return true;
                }
            }
        } else if r.kind == ResKind::RwLock {
            if let Some(&pid) = engine.net.place_of.get(&PlaceKey::RwLock(r.id)) {
                if matches!(
                    state.read_rwlock(pid),
                    Some(RwLockToken::Reading(_) | RwLockToken::Writing(_))
                ) {
                    return true;
                }
            }
        }
    }
    false
}

fn classify_spec_edge(
    sem: &SemProgram,
    engine: &PetriEngine,
    src: &NetState,
    dst: &NetState,
    label: &crate::sem::outcome::StepLabel,
) -> Option<SyncAction> {
    let fqn_of = |r: ResourceId| spec_resource_fqn(sem, r);

    match label.origin.phase {
        Phase::Statement => {
            let sid = label.origin.sid?;
            let stmt = sem.function(label.origin.function).body.get(sid)?;
            match &stmt.op {
                SemOp::MutexLock { resource } => {
                    // Only emit `mutex_lock` when the lock is actually acquired
                    // (Free -> Held), not when blocking into LockWait.
                    let pid = *engine.net.place_of.get(&PlaceKey::Mutex(*resource))?;
                    let was_free = matches!(src.read_mutex(pid), Some(MutexToken::Free));
                    let now_held = matches!(dst.read_mutex(pid), Some(MutexToken::Held(_)));
                    if was_free && now_held {
                        Some(SyncAction {
                            op: "mutex_lock".into(),
                            resource: fqn_of(*resource),
                        })
                    } else {
                        None
                    }
                }
                SemOp::MutexUnlock { resource } => Some(SyncAction {
                    op: "mutex_unlock".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::RwLockRead { resource } => {
                    if let Some(tid) = label.thread {
                        let blocked = dst
                            .store
                            .threads
                            .get(&tid)
                            .and_then(|t| t.blocked_at.as_ref())
                            .is_some();
                        if blocked {
                            return None;
                        }
                    }
                    Some(SyncAction {
                        op: "rwlock_read_lock".into(),
                        resource: fqn_of(*resource),
                    })
                }
                SemOp::RwLockWrite { resource } => {
                    if let Some(tid) = label.thread {
                        let blocked = dst
                            .store
                            .threads
                            .get(&tid)
                            .and_then(|t| t.blocked_at.as_ref())
                            .is_some();
                        if blocked {
                            return None;
                        }
                    }
                    Some(SyncAction {
                        op: "rwlock_write_lock".into(),
                        resource: fqn_of(*resource),
                    })
                }
                SemOp::RwLockUnlock { resource } => {
                    let op = engine
                        .net
                        .place_of
                        .get(&PlaceKey::RwLock(*resource))
                        .and_then(|pid| src.read_rwlock(*pid))
                        .map(|tok| match tok {
                            RwLockToken::Writing(_) => "rwlock_write_unlock",
                            RwLockToken::Reading(_) => "rwlock_read_unlock",
                            RwLockToken::Free => "rwlock_unlock",
                        })
                        .unwrap_or("rwlock_unlock");
                    Some(SyncAction {
                        op: op.into(),
                        resource: fqn_of(*resource),
                    })
                }
                SemOp::CondvarWait { condvar, .. } => Some(SyncAction {
                    op: "condvar_wait".into(),
                    resource: fqn_of(*condvar),
                }),
                SemOp::CondvarNotify { condvar } => Some(SyncAction {
                    op: "condvar_notify".into(),
                    resource: fqn_of(*condvar),
                }),
                SemOp::CondvarNotifyAll { condvar } => Some(SyncAction {
                    op: "condvar_notify_all".into(),
                    resource: fqn_of(*condvar),
                }),
                SemOp::SemaphoreAcquire { resource, .. } => {
                    let pid = *engine.net.place_of.get(&PlaceKey::Semaphore(*resource))?;
                    let before = src.read_data(pid).and_then(|v| v.as_int()).unwrap_or(0);
                    let after = dst.read_data(pid).and_then(|v| v.as_int()).unwrap_or(0);
                    if after < before {
                        Some(SyncAction {
                            op: "sem_acquire".into(),
                            resource: fqn_of(*resource),
                        })
                    } else {
                        None
                    }
                }
                SemOp::SemaphoreRelease { resource, .. } => Some(SyncAction {
                    op: "sem_release".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::ChannelSend { channel, .. } => {
                    // Only observable if not blocked in ChannelSend wait place.
                    if let Some(tid) = label.thread {
                        let blocked = dst
                            .store
                            .threads
                            .get(&tid)
                            .and_then(|t| t.blocked_at.as_ref())
                            .is_some();
                        if blocked {
                            return None;
                        }
                    }
                    Some(SyncAction {
                        op: "channel_send".into(),
                        resource: fqn_of(*channel),
                    })
                }
                SemOp::ChannelRecv { channel, .. } => {
                    if let Some(tid) = label.thread {
                        let blocked = dst
                            .store
                            .threads
                            .get(&tid)
                            .and_then(|t| t.blocked_at.as_ref())
                            .is_some();
                        if blocked {
                            return None;
                        }
                    }
                    Some(SyncAction {
                        op: "channel_recv".into(),
                        resource: fqn_of(*channel),
                    })
                }
                SemOp::AtomicLoad { resource, .. } => Some(SyncAction {
                    op: "atomic_load".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::AtomicStore { resource, .. } => Some(SyncAction {
                    op: "atomic_store".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::AtomicCas { resource, .. } => Some(SyncAction {
                    op: "atomic_cas".into(),
                    resource: fqn_of(*resource),
                }),
                _ => None,
            }
        }
        Phase::Reacquire | Phase::Wake => {
            // A thread resumed from a wait place: inspect the waking frame's statement!
            let frame_id = label.frame?;
            let frame = src.store.frames.get(&frame_id)?;
            let stmt = sem.function(frame.function).body.get(frame.pc)?;
            match &stmt.op {
                SemOp::MutexLock { resource } => Some(SyncAction {
                    op: "mutex_lock".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::RwLockRead { resource } => Some(SyncAction {
                    op: "rwlock_read_lock".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::RwLockWrite { resource } => Some(SyncAction {
                    op: "rwlock_write_lock".into(),
                    resource: fqn_of(*resource),
                }),
                // CondvarWait lock re-acquisition is internal to condvar_wait.
                SemOp::CondvarWait { .. } => None,
                SemOp::SemaphoreAcquire { resource, .. } => Some(SyncAction {
                    op: "sem_acquire".into(),
                    resource: fqn_of(*resource),
                }),
                SemOp::ChannelSend { channel, .. } => Some(SyncAction {
                    op: "channel_send".into(),
                    resource: fqn_of(*channel),
                }),
                SemOp::ChannelRecv { channel, .. } => Some(SyncAction {
                    op: "channel_recv".into(),
                    resource: fqn_of(*channel),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

// ──────────────────── Impl LTS Construction ────────────────────

fn build_impl_lts(
    impl_net: &ImplNet,
    canon: &ResourceCanonicalizer,
    module_scope: Option<&ModuleScope>,
) -> (Lts, bool) {
    if impl_net.states.is_empty() && !impl_net.places.is_empty() {
        return explore_impl_petri_net(impl_net, canon, module_scope);
    }

    let trans_by_id: HashMap<usize, &ImplTransition> =
        impl_net.transitions.iter().map(|t| (t.id, t)).collect();
    let state_idx_by_id: HashMap<usize, usize> = impl_net
        .states
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();

    let n = impl_net.states.len().max(1);
    let mut tau_edges: Vec<Vec<(usize, EdgeMeta)>> = vec![Vec::new(); n];
    let mut obs_edges: Vec<Vec<(SyncAction, usize, EdgeMeta)>> = vec![Vec::new(); n];
    let mut deadlock = vec![false; n];
    let mut terminal = vec![false; n];
    let mut holds_lock = vec![false; n];
    let mut initial = 0usize;

    for (i, st) in impl_net.states.iter().enumerate() {
        if st.initial {
            initial = i;
        }
        deadlock[i] = st.deadlock;
        terminal[i] = st.terminal;
        holds_lock[i] = !st.held_locks.is_empty();
    }

    for edge in &impl_net.edges {
        let Some(&u) = state_idx_by_id.get(&edge.from) else {
            continue;
        };
        let Some(&v) = state_idx_by_id.get(&edge.to) else {
            continue;
        };
        let t = trans_by_id.get(&edge.transition).copied();
        let meta = EdgeMeta {
            span: t.and_then(|tr| tr.span.clone()),
            hole_id: t.and_then(|tr| tr.hole_id.clone()),
        };
        if let Some(tr) = t {
            if tr.observable && is_sync_resource_op(&tr.op) {
                let res = tr
                    .resource
                    .as_deref()
                    .map(|r| canon.canonicalize(r))
                    .unwrap_or_default();
                let keep = module_scope
                    .map_or(true, |sc| sc.impl_edge_in_scope(&res, tr.fn_def_path.as_deref()));
                if keep {
                    obs_edges[u].push((
                        SyncAction {
                            op: tr.op.clone(),
                            resource: res,
                        },
                        v,
                        meta,
                    ));
                    continue;
                }
            }
        }
        tau_edges[u].push((v, meta));
    }

    (
        Lts {
            num_states: n,
            initial,
            tau_edges,
            obs_edges,
            deadlock,
            terminal,
            holds_lock,
        },
        false,
    )
}

/// Fallback Petri-net reachability exploration when `impl_sync_net.json`
/// supplies `places` + `transitions` without a pre-computed `states`/`edges` graph.
fn explore_impl_petri_net(
    impl_net: &ImplNet,
    canon: &ResourceCanonicalizer,
    module_scope: Option<&ModuleScope>,
) -> (Lts, bool) {
    const MAX_IMPL_STATES: usize = 50_000;

    let place_idx: HashMap<usize, usize> = impl_net
        .places
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id, i))
        .collect();
    let num_places = impl_net.places.len();
    let init_marking: Vec<usize> = impl_net.places.iter().map(|p| p.initial_tokens).collect();

    let lock_places: Vec<usize> = impl_net
        .places
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            matches!(
                p.resource_kind.as_deref(),
                Some("Mutex" | "mutex" | "RwLock" | "rwlock" | "Lock" | "lock")
            ) || p.class == "lock"
        })
        .map(|(i, _)| i)
        .collect();

    let mut marking_to_id: HashMap<Vec<usize>, usize> = HashMap::new();
    let mut markings: Vec<Vec<usize>> = Vec::new();
    let mut tau_edges: Vec<Vec<(usize, EdgeMeta)>> = Vec::new();
    let mut obs_edges: Vec<Vec<(SyncAction, usize, EdgeMeta)>> = Vec::new();
    let mut deadlock: Vec<bool> = Vec::new();
    let mut terminal: Vec<bool> = Vec::new();
    let mut holds_lock: Vec<bool> = Vec::new();
    let mut queue = VecDeque::new();
    let mut truncated = false;

    marking_to_id.insert(init_marking.clone(), 0);
    markings.push(init_marking.clone());
    tau_edges.push(Vec::new());
    obs_edges.push(Vec::new());
    deadlock.push(false);
    terminal.push(false);
    holds_lock.push(false);
    queue.push_back(0usize);

    while let Some(u) = queue.pop_front() {
        let m = markings[u].clone();
        holds_lock[u] = lock_places
            .iter()
            .any(|&pi| m[pi] < init_marking[pi]);

        let mut fired_any = false;
        for tr in &impl_net.transitions {
            // Check input token requirements.
            let mut need: HashMap<usize, usize> = HashMap::new();
            let mut valid = true;
            for &arc in &tr.inputs {
                if let Some(&pi) = place_idx.get(&arc.place()) {
                    *need.entry(pi).or_insert(0) += arc.weight();
                } else {
                    valid = false;
                    break;
                }
            }
            if !valid || need.iter().any(|(&pi, &cnt)| m[pi] < cnt) {
                continue;
            }
            fired_any = true;
            let mut next_m = m.clone();
            for (&pi, &cnt) in &need {
                next_m[pi] -= cnt;
            }
            for &arc in &tr.outputs {
                if let Some(&pi) = place_idx.get(&arc.place()) {
                    next_m[pi] += arc.weight();
                }
            }

            let v = if let Some(&existing) = marking_to_id.get(&next_m) {
                existing
            } else {
                if markings.len() >= MAX_IMPL_STATES {
                    truncated = true;
                    break;
                }
                let id = markings.len();
                marking_to_id.insert(next_m.clone(), id);
                markings.push(next_m);
                tau_edges.push(Vec::new());
                obs_edges.push(Vec::new());
                deadlock.push(false);
                terminal.push(false);
                holds_lock.push(false);
                queue.push_back(id);
                id
            };

            let meta = EdgeMeta {
                span: tr.span.clone(),
                hole_id: tr.hole_id.clone(),
            };
            if tr.observable && is_sync_resource_op(&tr.op) {
                let res = tr
                    .resource
                    .as_deref()
                    .map(|r| canon.canonicalize(r))
                    .unwrap_or_default();
                let keep = module_scope
                    .map_or(true, |sc| sc.impl_edge_in_scope(&res, tr.fn_def_path.as_deref()));
                if keep {
                    obs_edges[u].push((
                        SyncAction {
                            op: tr.op.clone(),
                            resource: res,
                        },
                        v,
                        meta,
                    ));
                } else {
                    tau_edges[u].push((v, meta));
                }
            } else {
                tau_edges[u].push((v, meta));
            }
        }

        if !fired_any {
            // Deadlock if any lock is held or any non-end control place still has tokens.
            let non_end_active = (0..num_places).any(|pi| {
                m[pi] > 0
                    && impl_net.places[pi].class == "control"
                    && !impl_net.places[pi].name.contains("end")
            });
            if holds_lock[u] || non_end_active {
                deadlock[u] = true;
            } else {
                terminal[u] = true;
            }
        }
        if truncated {
            break;
        }
    }

    (
        Lts {
            num_states: markings.len(),
            initial: 0,
            tau_edges,
            obs_edges,
            deadlock,
            terminal,
            holds_lock,
        },
        truncated,
    )
}

// ──────────────────── UniPN Stuttering Refinement Bridge ────────────────────

fn compute_tau_closure(lts: &Lts) -> Vec<BTreeSet<usize>> {
    let mut closures = vec![BTreeSet::new(); lts.num_states];
    for start in 0..lts.num_states {
        let mut seen = BTreeSet::new();
        let mut q = VecDeque::new();
        seen.insert(start);
        q.push_back(start);
        while let Some(u) = q.pop_front() {
            for (v, _) in &lts.tau_edges[u] {
                if seen.insert(*v) {
                    q.push_back(*v);
                }
            }
        }
        closures[start] = seen;
    }
    closures
}

fn compute_spec_weak_map(
    spec: &Lts,
    spec_tau: &[BTreeSet<usize>],
    collapse_notify_all: bool,
    collapse_rwlock_unlock: bool,
) -> Vec<BTreeMap<SyncAction, BTreeSet<usize>>> {
    let mut out = vec![BTreeMap::new(); spec.num_states];
    for q in 0..spec.num_states {
        let map = &mut out[q];
        for &q_mid in &spec_tau[q] {
            for (act, q_after, _) in &spec.obs_edges[q_mid] {
                let norm = SyncAction {
                    op: normalize_op_for_matching(
                        &act.op,
                        collapse_notify_all,
                        collapse_rwlock_unlock,
                    ),
                    resource: act.resource.clone(),
                };
                let entry = map.entry(norm).or_insert_with(BTreeSet::new);
                for &q_end in &spec_tau[*q_after] {
                    entry.insert(q_end);
                }
            }
        }
    }
    out
}

fn run_unipn_refinement(
    impl_lts: &Lts,
    spec_lts: &Lts,
    spec_has_lock_tau_cycle: bool,
    codegen_map: Option<&CodegenMap>,
) -> (
    Option<RefinementViolation>,
    Option<RefinementViolation>,
    Option<RefinementViolation>,
) {
    if impl_lts.num_states == 0 || spec_lts.num_states == 0 {
        return (None, None, None);
    }

    // Decide whether `spn`'s coarse notify or generic rwlock_unlock normalization applies.
    let impl_has_notify_all = impl_lts
        .obs_edges
        .iter()
        .flatten()
        .any(|(a, _, _)| a.op == "condvar_notify_all");
    let collapse_notify_all = !impl_has_notify_all;

    let impl_has_generic_rw_unlock = impl_lts
        .obs_edges
        .iter()
        .flatten()
        .any(|(a, _, _)| a.op == "rwlock_unlock");
    let collapse_rwlock_unlock = impl_has_generic_rw_unlock;

    let mut impl_edges: Vec<(usize, usize, Option<SyncAction>, usize)> = Vec::new();
    let mut impl_edge_info: Vec<(Option<SyncAction>, EdgeMeta)> = Vec::new();
    for u in 0..impl_lts.num_states {
        for (v, meta) in &impl_lts.tau_edges[u] {
            let tag = impl_edge_info.len();
            impl_edge_info.push((None, meta.clone()));
            impl_edges.push((u, *v, None, tag));
        }
        for (act, v, meta) in &impl_lts.obs_edges[u] {
            let tag = impl_edge_info.len();
            impl_edge_info.push((Some(act.clone()), meta.clone()));
            let norm = SyncAction {
                op: normalize_op_for_matching(
                    &act.op,
                    collapse_notify_all,
                    collapse_rwlock_unlock,
                ),
                resource: act.resource.clone(),
            };
            impl_edges.push((u, *v, Some(norm), tag));
        }
    }

    let mut spec_edges: Vec<(usize, usize, Option<SyncAction>)> = Vec::new();
    for u in 0..spec_lts.num_states {
        for (v, _) in &spec_lts.tau_edges[u] {
            spec_edges.push((u, *v, None));
        }
        for (act, v, _) in &spec_lts.obs_edges[u] {
            let norm = SyncAction {
                op: normalize_op_for_matching(
                    &act.op,
                    collapse_notify_all,
                    collapse_rwlock_unlock,
                ),
                resource: act.resource.clone(),
            };
            spec_edges.push((u, *v, Some(norm)));
        }
    }

    let res = check_stuttering_refinement(
        impl_lts.num_states,
        impl_lts.initial,
        &impl_edges,
        spec_lts.num_states,
        spec_lts.initial,
        &spec_edges,
        |s| impl_lts.deadlock[s],
        |s| spec_lts.deadlock[s],
        |s| impl_lts.holds_lock[s],
        spec_has_lock_tau_cycle,
    );

    // ── Map weak simulation violation ──
    let weak_sim_violation = if res.simulated {
        None
    } else {
        let spec_tau = compute_tau_closure(spec_lts);
        let spec_weak = compute_spec_weak_map(
            spec_lts,
            &spec_tau,
            collapse_notify_all,
            collapse_rwlock_unlock,
        );

        let mut witness_trace = Vec::new();
        let mut last_meta = EdgeMeta {
            span: None,
            hole_id: None,
        };
        let mut prefix_norm_actions: Vec<SyncAction> = Vec::new();

        if let Some(w) = &res.simulation_witness {
            for &tag in &w.impl_edge_tags {
                if let Some((act_opt, meta)) = impl_edge_info.get(tag) {
                    if let Some(act) = act_opt {
                        witness_trace.push(WitnessStep {
                            step: witness_trace.len(),
                            op: act.op.clone(),
                            resource: act.resource.clone(),
                            span: meta.span.clone(),
                        });
                        prefix_norm_actions.push(SyncAction {
                            op: normalize_op_for_matching(
                                &act.op,
                                collapse_notify_all,
                                collapse_rwlock_unlock,
                            ),
                            resource: act.resource.clone(),
                        });
                        last_meta = meta.clone();
                    }
                }
            }
        }

        // Compute the spec frontier right before the final failing action so we
        // report exact `expected_spec_ops`.
        let mut frontier: BTreeSet<usize> = spec_tau[spec_lts.initial].clone();
        let prefix_len = prefix_norm_actions.len().saturating_sub(1);
        for act in &prefix_norm_actions[..prefix_len] {
            let mut next_frontier = BTreeSet::new();
            for &q in &frontier {
                if let Some(dests) = spec_weak[q].get(act) {
                    next_frontier.extend(dests.iter().copied());
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for &q in &frontier {
            for spec_act in spec_weak[q].keys() {
                expected.insert(spec_act.display());
            }
        }

        let failed_display = res
            .simulation_witness
            .as_ref()
            .and_then(|w| w.failed_action.as_ref())
            .map(|a| a.display())
            .or_else(|| {
                witness_trace
                    .last()
                    .map(|s| format!("{}({})", s.op, s.resource))
            })
            .unwrap_or_else(|| "<step>".to_string());

        let (sid, req_clauses) = hole_metadata(last_meta.hole_id.as_deref(), codegen_map);
        Some(RefinementViolation {
            obligation: "O2_refinement".to_string(),
            kind: "unexpected_sync_step".to_string(),
            message: format!(
                "implementation performed unauthorized synchronization step '{failed_display}' after {} observable step(s)",
                witness_trace.len().saturating_sub(1)
            ),
            hole_id: last_meta.hole_id,
            sid,
            req_clauses,
            rust_span: last_meta.span,
            witness_trace,
            expected_spec_ops: expected.into_iter().collect(),
        })
    };

    // ── Map deadlock violation ──
    let deadlock_violation = if res.deadlocked_impl_states.is_empty() {
        None
    } else {
        let traces = reachable_states_with_trace(impl_lts);
        let mut dls = res.deadlocked_impl_states.clone();
        dls.sort_by_key(|&s| (traces.get(&s).map_or(usize::MAX, Vec::len), s));
        let dl = dls[0];
        let witness = traces.get(&dl).cloned().unwrap_or_default();
        Some(RefinementViolation {
            obligation: "O2_refinement".to_string(),
            kind: "deadlock_introduced".to_string(),
            message: format!(
                "implementation reaches a deadlock state (state {dl}) after {} observable step(s)",
                witness.len()
            ),
            hole_id: None,
            sid: None,
            req_clauses: Vec::new(),
            rust_span: witness.last().and_then(|s| s.span.clone()),
            witness_trace: witness,
            expected_spec_ops: Vec::new(),
        })
    };

    // ── Map lock-divergence violation ──
    let lock_div_violation = res.lock_divergences.first().map(|div| {
        let mut witness = Vec::new();
        for &tag in &div.prefix_edge_tags {
            if let Some((Some(act), meta)) = impl_edge_info.get(tag) {
                witness.push(WitnessStep {
                    step: witness.len(),
                    op: act.op.clone(),
                    resource: act.resource.clone(),
                    span: meta.span.clone(),
                });
            }
        }
        let mut chosen_meta = EdgeMeta {
            span: None,
            hole_id: None,
        };
        for &tag in &div.cycle_edge_tags {
            if let Some((_, meta)) = impl_edge_info.get(tag) {
                if meta.hole_id.is_some() {
                    chosen_meta = meta.clone();
                    break;
                }
                if chosen_meta.span.is_none() && meta.span.is_some() {
                    chosen_meta = meta.clone();
                }
            }
        }
        let cyc_state = div.cycle_states.first().copied().unwrap_or(0);
        let (sid, req_clauses) = hole_metadata(chosen_meta.hole_id.as_deref(), codegen_map);
        RefinementViolation {
            obligation: "O2_refinement".to_string(),
            kind: "lock_divergence".to_string(),
            message: format!(
                "reachable tau-cycle detected while holding lock(s) at implementation state {cyc_state}"
            ),
            hole_id: chosen_meta.hole_id,
            sid,
            req_clauses,
            rust_span: chosen_meta.span,
            witness_trace: witness,
            expected_spec_ops: Vec::new(),
        }
    });

    (weak_sim_violation, deadlock_violation, lock_div_violation)
}

// ──────────────────── Deadlock & Lock-Divergence Checks ────────────────────

fn reachable_states_with_trace(lts: &Lts) -> HashMap<usize, Vec<WitnessStep>> {
    let mut traces: HashMap<usize, Vec<WitnessStep>> = HashMap::new();
    let mut queue = VecDeque::new();
    traces.insert(lts.initial, Vec::new());
    queue.push_back(lts.initial);

    while let Some(u) = queue.pop_front() {
        let cur = traces[&u].clone();
        for (v, _) in &lts.tau_edges[u] {
            if !traces.contains_key(v) {
                traces.insert(*v, cur.clone());
                queue.push_back(*v);
            }
        }
        for (act, v, meta) in &lts.obs_edges[u] {
            if !traces.contains_key(v) {
                let mut next = cur.clone();
                next.push(WitnessStep {
                    step: next.len(),
                    op: act.op.clone(),
                    resource: act.resource.clone(),
                    span: meta.span.clone(),
                });
                traces.insert(*v, next);
                queue.push_back(*v);
            }
        }
    }
    traces
}

/// Returns `(cycle_state, optional_meta)` if the LTS has a reachable $\tau$-cycle
/// while holding at least one lock.
fn has_lock_holding_tau_cycle(lts: &Lts) -> Option<(usize, EdgeMeta)> {
    let traces = reachable_states_with_trace(lts);
    let nodes: Vec<usize> = traces
        .keys()
        .copied()
        .filter(|&s| lts.holds_lock[s])
        .collect();
    if nodes.is_empty() {
        return None;
    }
    let idx_of: HashMap<usize, usize> = nodes.iter().enumerate().map(|(i, &s)| (s, i)).collect();
    let n = nodes.len();
    let mut adj: Vec<Vec<(usize, EdgeMeta)>> = vec![Vec::new(); n];
    for (ui, &u) in nodes.iter().enumerate() {
        for (v, meta) in &lts.tau_edges[u] {
            if let Some(&vi) = idx_of.get(v) {
                adj[ui].push((vi, meta.clone()));
            }
        }
    }

    // Tarjan's SCC over lock-holding tau subgraph.
    struct State {
        index: usize,
        indices: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }
    fn strongconnect(v: usize, adj: &[Vec<(usize, EdgeMeta)>], st: &mut State) {
        st.indices[v] = Some(st.index);
        st.lowlink[v] = st.index;
        st.index += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        for (w, _) in &adj[v] {
            if st.indices[*w].is_none() {
                strongconnect(*w, adj, st);
                st.lowlink[v] = st.lowlink[v].min(st.lowlink[*w]);
            } else if st.on_stack[*w] {
                st.lowlink[v] = st.lowlink[v].min(st.indices[*w].unwrap());
            }
        }
        if st.lowlink[v] == st.indices[v].unwrap() {
            let mut scc = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on_stack[w] = false;
                scc.push(w);
                if w == v {
                    break;
                }
            }
            st.sccs.push(scc);
        }
    }

    let mut st = State {
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for v in 0..n {
        if st.indices[v].is_none() {
            strongconnect(v, &adj, &mut st);
        }
    }

    for scc in st.sccs {
        let scc_set: HashSet<usize> = scc.iter().copied().collect();
        let is_cycle = scc.len() >= 2
            || (scc.len() == 1 && adj[scc[0]].iter().any(|(w, _)| *w == scc[0]));
        if is_cycle {
            // Prefer an edge in the cycle carrying a hole_id or span.
            let mut chosen_meta = EdgeMeta {
                span: None,
                hole_id: None,
            };
            for &ui in &scc {
                for (vi, meta) in &adj[ui] {
                    if scc_set.contains(vi) {
                        if meta.hole_id.is_some() {
                            return Some((nodes[ui], meta.clone()));
                        }
                        if chosen_meta.span.is_none() && meta.span.is_some() {
                            chosen_meta = meta.clone();
                        }
                    }
                }
            }
            return Some((nodes[scc[0]], chosen_meta));
        }
    }

    None
}

// ──────────────────── Preserved Goals & Non-Triviality ────────────────────

fn collect_preserved_functions(pred: &PredicateSpec, out: &mut BTreeSet<String>) {
    match pred {
        PredicateSpec::FunctionCompleted { function }
        | PredicateSpec::FunctionCompletedAtLeast { function, .. } => {
            out.insert(function.clone());
        }
        PredicateSpec::And { predicates } | PredicateSpec::Or { predicates } => {
            for p in predicates {
                collect_preserved_functions(p, out);
            }
        }
        PredicateSpec::Not { .. } => {}
        _ => {}
    }
}

fn matches_completed_function(required: &str, completed: &[String]) -> bool {
    let short = required.rsplit("::").next().unwrap_or(required);
    let cf_short = format!("cf_{}", sanitize_ident(short));
    completed.iter().any(|c| {
        c == required
            || c == short
            || c == &cf_short
            || c.ends_with(&format!("::{short}"))
            || c.ends_with(&format!("::{cf_short}"))
    })
}

fn check_preserved_goals(
    impl_lts: &Lts,
    spec_lts: &Lts,
    impl_net: &ImplNet,
    contract: Option<&ContractSpec>,
    module_scope: Option<&ModuleScope>,
) -> Vec<RefinementViolation> {
    let mut violations = Vec::new();

    if let Some(c) = contract {
        let mut required_fns = BTreeSet::new();
        for p in &c.preserved {
            match p {
                PreservedSpec::Reachable { goal, .. } => {
                    collect_preserved_functions(goal, &mut required_fns);
                }
                PreservedSpec::Always { invariant, .. } => {
                    collect_preserved_functions(invariant, &mut required_fns);
                }
            }
        }
        for func in required_fns {
            if let Some(scope) = module_scope {
                if !scope.fn_in_scope(&func) {
                    continue;
                }
            }
            if !matches_completed_function(&func, &impl_net.completed_functions) {
                violations.push(RefinementViolation {
                    obligation: "O2_refinement".to_string(),
                    kind: "missing_preserved_goal".to_string(),
                    message: format!(
                        "preserved function '{func}' did not complete in implementation net"
                    ),
                    hole_id: None,
                    sid: None,
                    req_clauses: Vec::new(),
                    rust_span: None,
                    witness_trace: Vec::new(),
                    expected_spec_ops: Vec::new(),
                });
            }
        }
    }

    // Non-triviality: if spec has >= 1 reachable observable sync transition AND
    // cannot terminate via tau-only steps from initial state, then impl must
    // also have >= 1 reachable observable sync transition.
    let spec_tau = compute_tau_closure(spec_lts);
    let spec_has_obs = (0..spec_lts.num_states).any(|s| !spec_lts.obs_edges[s].is_empty());
    let spec_can_terminate_silently = spec_tau[spec_lts.initial]
        .iter()
        .any(|&s| spec_lts.terminal[s]);

    if spec_has_obs && !spec_can_terminate_silently {
        let impl_reach = reachable_states_with_trace(impl_lts);
        let impl_has_obs = impl_reach
            .keys()
            .any(|&s| !impl_lts.obs_edges[s].is_empty());
        if !impl_has_obs {
            violations.push(RefinementViolation {
                obligation: "O2_refinement".to_string(),
                kind: "missing_preserved_goal".to_string(),
                message: "implementation performs zero observable synchronization steps (vacuous no-op pruning)".to_string(),
                hole_id: None,
                sid: None,
                req_clauses: Vec::new(),
                rust_span: None,
                witness_trace: Vec::new(),
                expected_spec_ops: Vec::new(),
            });
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_scope_projects_imported_resource_by_origin_not_requires() {
        let prog: Program = serde_json::from_str(
            r#"{
                "program": "shared_res_mod",
                "version": "4.0.0",
                "entry": "app::main",
                "modules": [
                    {
                        "name": "app",
                        "provides": {"resources": [], "functions": ["main"]},
                        "requires": {"resources": [], "functions": ["storage::run_s", "worker::run"]},
                        "resources": [],
                        "protection": [],
                        "functions": [{
                            "name": "main",
                            "kind": "normal",
                            "body": [
                                {"sid": "s1", "kind": "scope", "funcs": ["storage::run_s", "worker::run"]},
                                {"sid": "s2", "kind": "return"}
                            ]
                        }]
                    },
                    {
                        "name": "storage",
                        "provides": {"resources": ["db_lock"], "functions": ["run_s"]},
                        "requires": {"resources": [], "functions": []},
                        "resources": [
                            {"name": "db_lock", "kind": "sync", "type": "Mutex", "mode": "Sync"}
                        ],
                        "protection": [],
                        "functions": [{
                            "name": "run_s",
                            "kind": "normal",
                            "body": [
                                {"sid": "s1", "kind": "mutex_lock", "resource": "db_lock"},
                                {"sid": "s2", "kind": "mutex_unlock", "resource": "db_lock"},
                                {"sid": "s3", "kind": "return"}
                            ]
                        }]
                    },
                    {
                        "name": "worker",
                        "provides": {"resources": [], "functions": ["run"]},
                        "requires": {"resources": ["storage::db_lock"], "functions": []},
                        "resources": [],
                        "protection": [],
                        "functions": [{
                            "name": "run",
                            "kind": "normal",
                            "body": [
                                {"sid": "s1", "kind": "mutex_lock", "resource": "storage::db_lock"},
                                {"sid": "s2", "kind": "mutex_unlock", "resource": "storage::db_lock"},
                                {"sid": "s3", "kind": "return"}
                            ]
                        }]
                    }
                ]
            }"#,
        )
        .expect("parse");

        // In `--focus-module worker` projection:
        // - `storage`'s transitions on `storage::db_lock` are silent (`observable: false`),
        // - `worker`'s transitions on `storage::db_lock` are observable (`observable: true`,
        //   with `fn_def_path: Some("cir_generated::worker::cf_run")`).
        let impl_net: ImplNet = serde_json::from_str(
            r#"{
                "schema": "concir-impl-net/1",
                "crate": "cir_generated",
                "focus_module": "worker",
                "states": [
                    {"id": 0, "initial": true, "terminal": false, "deadlock": false, "held_locks": []},
                    {"id": 1, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["storage::db_lock"]},
                    {"id": 2, "initial": false, "terminal": false, "deadlock": false, "held_locks": []},
                    {"id": 3, "initial": false, "terminal": false, "deadlock": false, "held_locks": ["storage::db_lock"]},
                    {"id": 4, "initial": false, "terminal": true, "deadlock": false, "held_locks": []}
                ],
                "transitions": [
                    {"id": 0, "observable": false, "op": "mutex_lock", "resource": "storage::db_lock", "fn_def_path": "cir_generated::storage::cf_run_s"},
                    {"id": 1, "observable": false, "op": "mutex_unlock", "resource": "storage::db_lock", "fn_def_path": "cir_generated::storage::cf_run_s"},
                    {"id": 2, "observable": true, "op": "mutex_lock", "resource": "storage::db_lock", "fn_def_path": "cir_generated::worker::cf_run"},
                    {"id": 3, "observable": true, "op": "mutex_unlock", "resource": "storage::db_lock", "fn_def_path": "cir_generated::worker::cf_run"}
                ],
                "edges": [
                    {"from": 0, "to": 1, "transition": 0},
                    {"from": 1, "to": 2, "transition": 1},
                    {"from": 2, "to": 3, "transition": 2},
                    {"from": 3, "to": 4, "transition": 3}
                ],
                "purity_violations": [],
                "completed_functions": ["worker::run"]
            }"#,
        )
        .expect("impl_net");

        let rep = verify_refinement_for_module(&prog, &impl_net, None, None, Some("worker"))
            .expect("verify_refinement_for_module");
        assert_eq!(
            rep.outcome, "pass",
            "expected worker modular refinement to pass with silent storage ops: {rep:?}"
        );
    }
}
