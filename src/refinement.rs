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

use crate::ast::Program;
use crate::codegen::CodegenMap;
use crate::explore::contract::{ContractSpec, PredicateSpec, PreservedSpec};
use crate::explore::explore;
use crate::fqn;
use crate::petri::net::{MutexToken, NetState, PlaceKey};
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

fn actions_match(impl_act: &SyncAction, spec_act: &SyncAction) -> bool {
    if impl_act.resource != spec_act.resource {
        return false;
    }
    if impl_act.op == spec_act.op {
        return true;
    }
    // `spn`'s TransitionType::Notify may emit `condvar_notify` for `notify_all`.
    if impl_act.op == "condvar_notify" && spec_act.op == "condvar_notify_all" {
        return true;
    }
    false
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

/// Run branch-pruning stuttering refinement checking.
pub fn verify_refinement(
    program: &Program,
    impl_net: &ImplNet,
    contract: Option<&ContractSpec>,
    codegen_map: Option<&CodegenMap>,
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
    let bounds = contract
        .map(|c| AnalysisBounds::from(&c.bounds))
        .unwrap_or_default();

    // 1. Explore specification Petri net.
    let engine = PetriEngine::new(&sem, bounds.clone());
    let spec_reach = explore(&engine, &bounds);
    let spec_lts = build_spec_lts(&sem, &engine, &spec_reach);

    // 2. Build (or explore) implementation LTS with canonicalized sync labels.
    let canon = ResourceCanonicalizer::new(&sem, codegen_map);
    let (impl_lts, impl_truncated) = build_impl_lts(impl_net, &canon);
    let any_truncated = impl_net.truncated || impl_truncated || spec_reach.truncated;

    let mut violations: Vec<RefinementViolation> = Vec::new();

    // ── Check 1: O1 Hole Purity ──
    for pv in &impl_net.purity_violations {
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
    let hole_purity = impl_net.purity_violations.is_empty();

    // ── Check 2: O2 Weak / Stuttering Simulation ──
    let weak_sim_violation = check_weak_simulation(&impl_lts, &spec_lts, codegen_map);
    let weak_simulation = weak_sim_violation.is_none();
    if let Some(v) = weak_sim_violation {
        violations.push(v);
    }

    // ── Check 3a: Deadlock Freedom ──
    let deadlock_violation = check_deadlock_free(&impl_lts);
    let deadlock_free = deadlock_violation.is_none();
    if let Some(v) = deadlock_violation {
        violations.push(v);
    }

    // ── Check 3b: Lock-Divergence Freedom ──
    let spec_has_lock_tau_cycle = has_lock_holding_tau_cycle(&spec_lts).is_some();
    let lock_div_violation = if !spec_has_lock_tau_cycle {
        check_lock_divergence_free(&impl_lts, codegen_map)
    } else {
        None
    };
    let lock_divergence_free = lock_div_violation.is_none();
    if let Some(v) = lock_div_violation {
        violations.push(v);
    }

    // ── Check 4: Preserved Goals & Non-Triviality ──
    let preserved_violations = check_preserved_goals(&impl_lts, &spec_lts, impl_net, contract);
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
                obs_edges[u].push((action, *v, empty_meta.clone()));
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

fn build_impl_lts(impl_net: &ImplNet, canon: &ResourceCanonicalizer) -> (Lts, bool) {
    if impl_net.states.is_empty() && !impl_net.places.is_empty() {
        return explore_impl_petri_net(impl_net, canon);
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
fn explore_impl_petri_net(impl_net: &ImplNet, canon: &ResourceCanonicalizer) -> (Lts, bool) {
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

// ──────────────────── Weak Simulation & Witness Trace ────────────────────

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
) -> Vec<BTreeMap<SyncAction, BTreeSet<usize>>> {
    let mut out = vec![BTreeMap::new(); spec.num_states];
    for q in 0..spec.num_states {
        let map = &mut out[q];
        for &q_mid in &spec_tau[q] {
            for (act, q_after, _) in &spec.obs_edges[q_mid] {
                let entry = map.entry(act.clone()).or_insert_with(BTreeSet::new);
                for &q_end in &spec_tau[*q_after] {
                    entry.insert(q_end);
                }
            }
        }
    }
    out
}

fn spec_matching_targets(
    spec_weak_q: &BTreeMap<SyncAction, BTreeSet<usize>>,
    impl_act: &SyncAction,
) -> BTreeSet<usize> {
    let mut targets = BTreeSet::new();
    for (spec_act, dests) in spec_weak_q {
        if actions_match(impl_act, spec_act) {
            targets.extend(dests.iter().copied());
        }
    }
    targets
}

fn compute_impl_weak_steps(
    impl_lts: &Lts,
    impl_tau: &[BTreeSet<usize>],
) -> Vec<Vec<(SyncAction, usize, EdgeMeta)>> {
    let mut out = vec![Vec::new(); impl_lts.num_states];
    for p in 0..impl_lts.num_states {
        let mut seen: BTreeSet<(SyncAction, usize)> = BTreeSet::new();
        for &p_mid in &impl_tau[p] {
            for (act, p_next, meta) in &impl_lts.obs_edges[p_mid] {
                if seen.insert((act.clone(), *p_next)) {
                    out[p].push((act.clone(), *p_next, meta.clone()));
                }
            }
        }
    }
    out
}

fn check_weak_simulation(
    impl_lts: &Lts,
    spec_lts: &Lts,
    codegen_map: Option<&CodegenMap>,
) -> Option<RefinementViolation> {
    if impl_lts.num_states == 0 || spec_lts.num_states == 0 {
        return None;
    }

    let impl_tau = compute_tau_closure(impl_lts);
    let spec_tau = compute_tau_closure(spec_lts);
    let impl_weak = compute_impl_weak_steps(impl_lts, &impl_tau);
    let spec_weak = compute_spec_weak_map(spec_lts, &spec_tau);

    // 1. Trace-refinement BFS: finds the shortest distinguishing observable trace
    //    whenever impl can perform an observable sequence not allowed by spec.
    if let Some(v) = bfs_trace_refinement(
        impl_lts.initial,
        spec_lts.initial,
        &impl_weak,
        &spec_weak,
        &spec_tau,
        codegen_map,
    ) {
        return Some(v);
    }

    // 2. Greatest fixed-point weak simulation (branching-time refinement) when
    //    the product size is tractable.
    let product = impl_lts.num_states.saturating_mul(spec_lts.num_states);
    if product <= 250_000 {
        let mut sim = vec![vec![true; spec_lts.num_states]; impl_lts.num_states];
        let mut changed = true;
        while changed {
            changed = false;
            for p in 0..impl_lts.num_states {
                for q in 0..spec_lts.num_states {
                    if !sim[p][q] {
                        continue;
                    }
                    let mut ok = true;
                    for (act, p_next, _) in &impl_weak[p] {
                        let targets = spec_matching_targets(&spec_weak[q], act);
                        if !targets.iter().any(|&q_next| sim[*p_next][q_next]) {
                            ok = false;
                            break;
                        }
                    }
                    if !ok {
                        sim[p][q] = false;
                        changed = true;
                    }
                }
            }
        }

        // Initial pair holds if any tau-successor of spec_init simulates impl_init.
        let init_simulated = spec_tau[spec_lts.initial]
            .iter()
            .any(|&q0| sim[impl_lts.initial][q0]);
        if !init_simulated {
            // Find shortest witness path to a non-simulable step from (impl_init, spec_init).
            return Some(branching_witness(
                impl_lts.initial,
                spec_lts.initial,
                &impl_weak,
                &spec_weak,
                &sim,
                codegen_map,
            ));
        }
    }

    None
}

fn bfs_trace_refinement(
    impl_init: usize,
    spec_init: usize,
    impl_weak: &[Vec<(SyncAction, usize, EdgeMeta)>],
    spec_weak: &[BTreeMap<SyncAction, BTreeSet<usize>>],
    spec_tau: &[BTreeSet<usize>],
    codegen_map: Option<&CodegenMap>,
) -> Option<RefinementViolation> {
    const MAX_FRONTIER_STATES: usize = 50_000;

    let init_frontier = spec_tau[spec_init].clone();
    let mut visited: HashSet<(usize, BTreeSet<usize>)> = HashSet::new();
    let mut queue: VecDeque<(usize, BTreeSet<usize>, Vec<WitnessStep>)> = VecDeque::new();

    visited.insert((impl_init, init_frontier.clone()));
    queue.push_back((impl_init, init_frontier, Vec::new()));

    while let Some((p, frontier, trace)) = queue.pop_front() {
        for (act, p_next, meta) in &impl_weak[p] {
            let mut next_frontier = BTreeSet::new();
            for &q in &frontier {
                next_frontier.extend(spec_matching_targets(&spec_weak[q], act));
            }
            let mut step_trace = trace.clone();
            step_trace.push(WitnessStep {
                step: step_trace.len(),
                op: act.op.clone(),
                resource: act.resource.clone(),
                span: meta.span.clone(),
            });

            if next_frontier.is_empty() {
                let mut expected: BTreeSet<String> = BTreeSet::new();
                for &q in &frontier {
                    for spec_act in spec_weak[q].keys() {
                        expected.insert(spec_act.display());
                    }
                }
                let (sid, req_clauses) = hole_metadata(meta.hole_id.as_deref(), codegen_map);
                return Some(RefinementViolation {
                    obligation: "O2_refinement".to_string(),
                    kind: "unexpected_sync_step".to_string(),
                    message: format!(
                        "implementation performed unauthorized synchronization step '{}' after {} observable step(s)",
                        act.display(),
                        trace.len()
                    ),
                    hole_id: meta.hole_id.clone(),
                    sid,
                    req_clauses,
                    rust_span: meta.span.clone(),
                    witness_trace: step_trace,
                    expected_spec_ops: expected.into_iter().collect(),
                });
            }

            if visited.len() < MAX_FRONTIER_STATES
                && visited.insert((*p_next, next_frontier.clone()))
            {
                queue.push_back((*p_next, next_frontier, step_trace));
            }
        }
    }

    None
}

fn branching_witness(
    impl_init: usize,
    spec_init: usize,
    impl_weak: &[Vec<(SyncAction, usize, EdgeMeta)>],
    spec_weak: &[BTreeMap<SyncAction, BTreeSet<usize>>],
    sim: &[Vec<bool>],
    codegen_map: Option<&CodegenMap>,
) -> RefinementViolation {
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut queue: VecDeque<(usize, usize, Vec<WitnessStep>)> = VecDeque::new();
    visited.insert((impl_init, spec_init));
    queue.push_back((impl_init, spec_init, Vec::new()));

    while let Some((p, q, trace)) = queue.pop_front() {
        for (act, p_next, meta) in &impl_weak[p] {
            let targets = spec_matching_targets(&spec_weak[q], act);
            if !targets.iter().any(|&q_next| sim[*p_next][q_next]) {
                let mut step_trace = trace.clone();
                step_trace.push(WitnessStep {
                    step: step_trace.len(),
                    op: act.op.clone(),
                    resource: act.resource.clone(),
                    span: meta.span.clone(),
                });
                let expected: Vec<String> = spec_weak[q].keys().map(|a| a.display()).collect();
                let (sid, req_clauses) = hole_metadata(meta.hole_id.as_deref(), codegen_map);
                return RefinementViolation {
                    obligation: "O2_refinement".to_string(),
                    kind: "unexpected_sync_step".to_string(),
                    message: format!(
                        "branching weak simulation failed at step '{}'",
                        act.display()
                    ),
                    hole_id: meta.hole_id.clone(),
                    sid,
                    req_clauses,
                    rust_span: meta.span.clone(),
                    witness_trace: step_trace,
                    expected_spec_ops: expected,
                };
            }
            for q_next in targets {
                if !sim[*p_next][q_next] && visited.insert((*p_next, q_next)) {
                    let mut next_trace = trace.clone();
                    next_trace.push(WitnessStep {
                        step: next_trace.len(),
                        op: act.op.clone(),
                        resource: act.resource.clone(),
                        span: meta.span.clone(),
                    });
                    queue.push_back((*p_next, q_next, next_trace));
                }
            }
        }
    }

    RefinementViolation {
        obligation: "O2_refinement".to_string(),
        kind: "unexpected_sync_step".to_string(),
        message: "implementation is not weakly simulated by specification".to_string(),
        hole_id: None,
        sid: None,
        req_clauses: Vec::new(),
        rust_span: None,
        witness_trace: Vec::new(),
        expected_spec_ops: Vec::new(),
    }
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

fn check_deadlock_free(impl_lts: &Lts) -> Option<RefinementViolation> {
    let traces = reachable_states_with_trace(impl_lts);
    let mut deadlocks: Vec<usize> = traces
        .keys()
        .copied()
        .filter(|&s| impl_lts.deadlock[s])
        .collect();
    deadlocks.sort_by_key(|&s| (traces[&s].len(), s));
    let dl = deadlocks.first().copied()?;
    let witness = traces[&dl].clone();
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

fn check_lock_divergence_free(
    impl_lts: &Lts,
    codegen_map: Option<&CodegenMap>,
) -> Option<RefinementViolation> {
    let (cyc_state, meta) = has_lock_holding_tau_cycle(impl_lts)?;
    let traces = reachable_states_with_trace(impl_lts);
    let witness = traces.get(&cyc_state).cloned().unwrap_or_default();
    let (sid, req_clauses) = hole_metadata(meta.hole_id.as_deref(), codegen_map);
    Some(RefinementViolation {
        obligation: "O2_refinement".to_string(),
        kind: "lock_divergence".to_string(),
        message: format!(
            "reachable tau-cycle detected while holding lock(s) at implementation state {cyc_state}"
        ),
        hole_id: meta.hole_id,
        sid,
        req_clauses,
        rust_span: meta.span,
        witness_trace: witness,
        expected_spec_ops: Vec::new(),
    })
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
