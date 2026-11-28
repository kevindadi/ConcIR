use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::Serialize;
use unipn::analysis::temporal::{
    check_ctl_graph, check_ltl_graph, explain_ctl, explain_ltl, CtlFormula, FairnessConstraint,
    LtlFormula,
};
use unipn::cvn::expr::{BoolExpr, CmpOp as UnipnCmpOp, Expr as UnipnExpr, Op as UnipnOp, Val};
use unipn::hln::{
    state_key, ControlToken, HlnBuilder, HlnFrame, HlnNet, HlnState, HlnTransition,
    HlnTransitionKind, HlnVal,
};
use unipn::net::{ControlSub, PlaceId};

use concir_ir::ast::{BaseType, ComplexBaseType};
use concir_ir::env::{NameEnv, Slot, SlotKind};
use concir_ir::expr::{self, BinOp, CmpOp, Expr, Lit};
pub use concir_ir::req_ir::{
    MilestoneSpec, ReqCheckReport, ReqDiagnostic, ReqProgram, ReqPropertyResult, ReqStep, RoleSpec,
    StateVarSpec, REQ_CHECK_SCHEMA, REQ_IR_SCHEMA,
};
use concir_sem::value::{default_value, from_json, within_type, Value};

use crate::contract::patterns::ConstraintPattern;
use crate::contract::{ContractSpec, PredicateSpec, PropertySpec};

fn diag(code: &str, message: impl Into<String>, location: Option<String>) -> ReqDiagnostic {
    ReqDiagnostic {
        code: code.to_string(),
        message: message.into(),
        location,
    }
}

fn build_name_env(req: &ReqProgram) -> NameEnv {
    let mut slots = HashMap::new();
    for sv in &req.state_vars {
        slots.insert(
            sv.name.clone(),
            Slot {
                kind: SlotKind::Var,
                ty: Some(sv.ty.clone()),
                modeled: true,
            },
        );
    }
    NameEnv::from_slots(slots)
}

fn is_bounded_var_type(ty: &BaseType) -> bool {
    match ty {
        BaseType::Primitive(p) => p == "Bool" || p == "Int",
        BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) => lo <= hi,
        BaseType::Complex(ComplexBaseType::Enum(vs)) => !vs.is_empty(),
        _ => false,
    }
}

/// Statically validate a [`ReqProgram`].
pub fn validate_req_program(req: &ReqProgram) -> Vec<ReqDiagnostic> {
    let mut diags = Vec::new();
    if req.schema != REQ_IR_SCHEMA {
        diags.push(diag(
            "R001",
            format!("unexpected req_ir schema '{}', expected '{REQ_IR_SCHEMA}'", req.schema),
            None,
        ));
    }
    if req.name.trim().is_empty() {
        diags.push(diag("R002", "req_ir 'name' must not be empty", None));
    }
    if req.roles.is_empty() {
        diags.push(diag("R003", "req_ir must declare at least one role", None));
    }
    if req.steps.is_empty() {
        diags.push(diag("R004", "req_ir must declare at least one step", None));
    }

    let mut role_states: HashMap<String, HashSet<String>> = HashMap::new();
    for role in &req.roles {
        if role.name.trim().is_empty() {
            diags.push(diag("R010", "role name must not be empty", None));
            continue;
        }
        if role_states.contains_key(&role.name) {
            diags.push(diag(
                "R011",
                format!("duplicate role '{}'", role.name),
                Some(role.name.clone()),
            ));
            continue;
        }
        if role.instances == 0 {
            diags.push(diag(
                "R012",
                format!("role '{}' must have instances >= 1", role.name),
                Some(role.name.clone()),
            ));
        }
        let state_set: HashSet<String> = role.states.iter().cloned().collect();
        if state_set.is_empty() {
            diags.push(diag(
                "R013",
                format!("role '{}' must declare at least one state", role.name),
                Some(role.name.clone()),
            ));
        } else if !state_set.contains(&role.init_state) {
            diags.push(diag(
                "R014",
                format!(
                    "role '{}' init_state '{}' is not in states",
                    role.name, role.init_state
                ),
                Some(role.name.clone()),
            ));
        }
        for ts in &role.terminal_states {
            if !state_set.contains(ts) {
                diags.push(diag(
                    "R015",
                    format!(
                        "role '{}' terminal_state '{}' is not in states",
                        role.name, ts
                    ),
                    Some(role.name.clone()),
                ));
            }
        }
        role_states.insert(role.name.clone(), state_set);
    }

    let mut var_types: HashMap<String, BaseType> = HashMap::new();
    for sv in &req.state_vars {
        if sv.name.trim().is_empty() {
            diags.push(diag("R020", "state_var name must not be empty", None));
            continue;
        }
        if var_types.contains_key(&sv.name) {
            diags.push(diag(
                "R021",
                format!("duplicate state_var '{}'", sv.name),
                Some(sv.name.clone()),
            ));
            continue;
        }
        if !is_bounded_var_type(&sv.ty) {
            diags.push(diag(
                "R022",
                format!(
                    "state_var '{}' has unsupported type {:?}; expected Bool, BoundedInt [lo, hi], or non-empty Enum",
                    sv.name, sv.ty
                ),
                Some(sv.name.clone()),
            ));
            continue;
        }
        if let Some(init_json) = &sv.init {
            if let Err(e) = from_json(init_json, &sv.ty) {
                diags.push(diag(
                    "R023",
                    format!("state_var '{}' invalid init: {e}", sv.name),
                    Some(sv.name.clone()),
                ));
            }
        }
        var_types.insert(sv.name.clone(), sv.ty.clone());
    }

    let mut milestone_names: HashSet<String> = HashSet::new();
    for m in &req.milestones {
        if m.name.trim().is_empty() {
            diags.push(diag("R030", "milestone name must not be empty", None));
            continue;
        }
        if !milestone_names.insert(m.name.clone()) {
            diags.push(diag(
                "R031",
                format!("duplicate milestone '{}'", m.name),
                Some(m.name.clone()),
            ));
        }
    }

    let env = build_name_env(req);
    let mut step_ids = HashSet::new();
    for step in &req.steps {
        let loc = Some(format!("{}::{}", step.role, step.id));
        if step.id.trim().is_empty() {
            diags.push(diag("R040", "step id must not be empty", loc.clone()));
        } else if !step_ids.insert(step.id.clone()) {
            diags.push(diag(
                "R041",
                format!("duplicate step id '{}'", step.id),
                loc.clone(),
            ));
        }
        match role_states.get(&step.role) {
            None => diags.push(diag(
                "R042",
                format!("step '{}' references unknown role '{}'", step.id, step.role),
                loc.clone(),
            )),
            Some(states) => {
                if !states.contains(&step.from_state) {
                    diags.push(diag(
                        "R043",
                        format!(
                            "step '{}' from_state '{}' is not declared on role '{}'",
                            step.id, step.from_state, step.role
                        ),
                        loc.clone(),
                    ));
                }
                if !states.contains(&step.to_state) {
                    diags.push(diag(
                        "R044",
                        format!(
                            "step '{}' to_state '{}' is not declared on role '{}'",
                            step.id, step.to_state, step.role
                        ),
                        loc.clone(),
                    ));
                }
            }
        }
        if let Some(ms) = &step.milestone {
            if ms.trim().is_empty() {
                diags.push(diag(
                    "R045",
                    format!("step '{}' has empty milestone name", step.id),
                    loc.clone(),
                ));
            } else if !milestone_names.contains(ms) {
                diags.push(diag(
                    "R046",
                    format!(
                        "step '{}' references undeclared milestone '{ms}'",
                        step.id
                    ),
                    loc.clone(),
                ));
            }
        }
        if let Some(guard_str) = &step.guard {
            if let Err(e) = parse_guard_bool_expr(guard_str, &env) {
                diags.push(diag(
                    "R047",
                    format!("step '{}' invalid guard '{guard_str}': {e}", step.id),
                    loc.clone(),
                ));
            }
        }
        for upd in &step.updates {
            let Some(ty) = var_types.get(&upd.target) else {
                diags.push(diag(
                    "R048",
                    format!(
                        "step '{}' update references unknown state_var '{}'",
                        step.id, upd.target
                    ),
                    loc.clone(),
                ));
                continue;
            };
            match expr::parse(&upd.expr, &env) {
                Err(e) => diags.push(diag(
                    "R049",
                    format!(
                        "step '{}' invalid update expr '{}': {}",
                        step.id, upd.expr, e.message
                    ),
                    loc.clone(),
                )),
                Ok(parsed) => match expr::type_of(&parsed, &env) {
                    Err(e) => diags.push(diag(
                        "R049",
                        format!(
                            "step '{}' update expr '{}' type error: {}",
                            step.id, upd.expr, e.message
                        ),
                        loc.clone(),
                    )),
                    Ok(got) => {
                        if let Err(e) = expr::assignable(got.as_ref(), ty, &parsed) {
                            diags.push(diag(
                                "R049",
                                format!(
                                    "step '{}' cannot assign '{}' to '{}': {}",
                                    step.id, upd.expr, upd.target, e.message
                                ),
                                loc.clone(),
                            ));
                        }
                    }
                },
            }
        }
    }

    diags
}

fn value_to_hln(v: &Value) -> HlnVal {
    match v {
        Value::Bool(b) => HlnVal::Bool(*b),
        Value::Int(i) => HlnVal::Int(*i),
        Value::Float(f) => HlnVal::Str(f.to_string()),
        Value::Str(s) => HlnVal::Str(s.clone()),
        Value::Enum(s) => HlnVal::Enum(s.clone()),
        Value::Struct(_) | Value::Array(_) => HlnVal::Str(v.canonical()),
    }
}

fn hln_to_value(v: &HlnVal) -> Value {
    match v {
        HlnVal::Bool(b) => Value::Bool(*b),
        HlnVal::Int(i) => Value::Int(*i),
        HlnVal::Float(bits) => Value::Float(f64::from_bits(*bits)),
        HlnVal::Str(s) => Value::Str(s.clone()),
        HlnVal::Enum(s) => Value::Enum(s.clone()),
        HlnVal::Struct(fields) => Value::Struct(
            fields
                .iter()
                .map(|(k, val)| (k.clone(), hln_to_value(val)))
                .collect(),
        ),
        HlnVal::Array(items) => Value::Array(items.iter().map(hln_to_value).collect()),
        HlnVal::Unknown => Value::Int(0),
    }
}

fn to_unipn_expr(e: &Expr) -> Result<UnipnExpr, String> {
    Ok(match e {
        Expr::Lit(Lit::Bool(b)) => UnipnExpr::Lit(Val::bool(*b)),
        Expr::Lit(Lit::Int(i)) => UnipnExpr::Lit(Val::int(*i)),
        Expr::Lit(Lit::Float(f)) => UnipnExpr::Lit(Val::float(*f)),
        Expr::Lit(Lit::String(s)) => UnipnExpr::Lit(Val::string(s.clone())),
        Expr::Lit(Lit::Enum(s)) => UnipnExpr::Lit(Val::enum_val(s.clone())),
        Expr::Name(n) => UnipnExpr::Ref(n.clone()),
        Expr::UnaryNeg(inner) => UnipnExpr::BinOp {
            op: UnipnOp::Sub,
            lhs: Box::new(UnipnExpr::Lit(Val::int(0))),
            rhs: Box::new(to_unipn_expr(inner)?),
        },
        Expr::BinOp { op, lhs, rhs } => UnipnExpr::BinOp {
            op: match op {
                BinOp::Add => UnipnOp::Add,
                BinOp::Sub => UnipnOp::Sub,
                BinOp::Mul => UnipnOp::Mul,
                BinOp::Div => UnipnOp::Div,
                BinOp::Mod => UnipnOp::Mod,
            },
            lhs: Box::new(to_unipn_expr(lhs)?),
            rhs: Box::new(to_unipn_expr(rhs)?),
        },
        Expr::Cmp { .. } | Expr::Field { .. } | Expr::Struct { .. } => {
            return Err("comparison/field/struct expressions are not allowed inside arithmetic RHS".into())
        }
    })
}

fn split_top_level<'a>(s: &'a str, sep: &str) -> Option<Vec<&'a str>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut last = 0usize;
    let bytes = s.as_bytes();
    let sep_b = sep.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => depth -= 1,
            _ if depth == 0 && bytes[i..].starts_with(sep_b) => {
                parts.push(&s[last..i]);
                i += sep.len();
                last = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(&s[last..]);
        Some(parts)
    }
}

fn parse_guard_bool_expr(input: &str, env: &NameEnv) -> Result<BoolExpr, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed == "true" {
        return Ok(BoolExpr::True);
    }
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        // Check if outer parens match each other.
        let mut depth = 0i32;
        let mut outer_wraps = true;
        for (i, ch) in trimmed.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i + ch.len_utf8() < trimmed.len() {
                        outer_wraps = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if outer_wraps {
            return parse_guard_bool_expr(&trimmed[1..trimmed.len() - 1], env);
        }
    }
    if let Some(parts) = split_top_level(trimmed, "||") {
        let mut acc = parse_guard_bool_expr(parts[0], env)?;
        for p in &parts[1..] {
            acc = BoolExpr::Or(Box::new(acc), Box::new(parse_guard_bool_expr(p, env)?));
        }
        return Ok(acc);
    }
    if let Some(parts) = split_top_level(trimmed, "&&") {
        let mut acc = parse_guard_bool_expr(parts[0], env)?;
        for p in &parts[1..] {
            acc = BoolExpr::And(Box::new(acc), Box::new(parse_guard_bool_expr(p, env)?));
        }
        return Ok(acc);
    }
    if let Some(rest) = trimmed.strip_prefix('!') {
        if !rest.starts_with('=') {
            return Ok(BoolExpr::Not(Box::new(parse_guard_bool_expr(rest, env)?)));
        }
    }
    let parsed = expr::parse(trimmed, env).map_err(|e| e.message)?;
    match parsed {
        Expr::Lit(Lit::Bool(true)) => Ok(BoolExpr::True),
        Expr::Lit(Lit::Bool(false)) => Ok(BoolExpr::Not(Box::new(BoolExpr::True))),
        Expr::Name(n) => {
            let Some(slot) = env.get(&n) else {
                return Err(format!("undefined name '{n}' in guard"));
            };
            if slot.ty != Some(BaseType::Primitive("Bool".into())) {
                return Err(format!("guard identifier '{n}' must have Bool type"));
            }
            Ok(BoolExpr::Cmp {
                op: UnipnCmpOp::Eq,
                lhs: Box::new(UnipnExpr::Ref(n)),
                rhs: Box::new(UnipnExpr::Lit(Val::bool(true))),
            })
        }
        Expr::Cmp { op, lhs, rhs } => Ok(BoolExpr::Cmp {
            op: match op {
                CmpOp::Eq => UnipnCmpOp::Eq,
                CmpOp::Ne => UnipnCmpOp::Ne,
                CmpOp::Lt => UnipnCmpOp::Lt,
                CmpOp::Le => UnipnCmpOp::Le,
                CmpOp::Gt => UnipnCmpOp::Gt,
                CmpOp::Ge => UnipnCmpOp::Ge,
            },
            lhs: Box::new(to_unipn_expr(&lhs)?),
            rhs: Box::new(to_unipn_expr(&rhs)?),
        }),
        _ => Err("guard must evaluate to Bool or a comparison".into()),
    }
}

pub struct LoweredReqHln {
    pub net: HlnNet,
    pub initial: HlnState,
    pub var_ids: BTreeMap<String, usize>,
    pub var_types: BTreeMap<usize, BaseType>,
    pub terminal_places: BTreeSet<PlaceId>,
    pub milestone_sites: BTreeMap<String, Vec<(usize, usize)>>,
    pub step_ids: BTreeMap<unipn::net::TransitionId, String>,
}

pub trait ReqProgramHln {
    fn lower_to_hln(&self) -> Result<LoweredReqHln, String>;
}

impl ReqProgramHln for ReqProgram {
    /// Lower this requirement net into a `UniPN` [`HlnNet`] + initial [`HlnState`].
    fn lower_to_hln(&self) -> Result<LoweredReqHln, String> {
        let diags = validate_req_program(self);
        if let Some(first) = diags.first() {
            return Err(format!("{}: {}", first.code, first.message));
        }

        let env = build_name_env(self);
        let mut builder = HlnBuilder::new();
        let mut place_map: HashMap<(String, String), PlaceId> = HashMap::new();
        let mut terminal_places: BTreeSet<PlaceId> = BTreeSet::new();
        let mut role_idx_of: HashMap<String, usize> = HashMap::new();

        // 1. Role control places & initial colored role tokens.
        let mut next_tid = 0u32;
        let mut next_fid = 0u32;
        for (r_idx, role) in self.roles.iter().enumerate() {
            role_idx_of.insert(role.name.clone(), r_idx);
            let terms: HashSet<&str> = role.terminal_states.iter().map(String::as_str).collect();
            for st in &role.states {
                let sub = if st == &role.init_state {
                    ControlSub::FunctionStart
                } else if terms.contains(st.as_str()) {
                    ControlSub::FunctionEnd
                } else {
                    ControlSub::BasicBlock
                };
                let pid = builder.add_control_place(format!("{}::{st}", role.name), sub);
                place_map.insert((role.name.clone(), st.clone()), pid);
                if terms.contains(st.as_str()) {
                    terminal_places.insert(pid);
                }
            }
            let init_pid = place_map[&(role.name.clone(), role.init_state.clone())];
            for _inst in 0..role.instances {
                let tok = ControlToken {
                    tid: next_tid,
                    entry_func: r_idx,
                    stack: vec![HlnFrame {
                        frame_id: next_fid,
                        func: r_idx,
                        pc: 0,
                        locals: BTreeMap::new(),
                        handles: BTreeMap::new(),
                        ret: None,
                    }],
                    handle_children: BTreeMap::new(),
                    held_locks: BTreeMap::new(),
                    parent_scope: None,
                    wait_tag: None,
                };
                next_tid += 1;
                next_fid += 1;
                builder.spawn_initial_token(init_pid, tok);
            }
        }

        // 2. Bounded state variable places.
        let mut var_ids: BTreeMap<String, usize> = BTreeMap::new();
        let mut var_types: BTreeMap<usize, BaseType> = BTreeMap::new();
        let mut var_places: BTreeMap<String, PlaceId> = BTreeMap::new();
        for (vid, sv) in self.state_vars.iter().enumerate() {
            let init_val = match &sv.init {
                Some(j) => from_json(j, &sv.ty)?,
                None => default_value(&sv.ty),
            };
            let pid = builder.add_var(vid, sv.name.clone(), value_to_hln(&init_val));
            if let BaseType::Complex(ComplexBaseType::BoundedInt { lo, hi }) = &sv.ty {
                builder.set_variable_domain(sv.name.clone(), *lo, *hi);
            }
            var_ids.insert(sv.name.clone(), vid);
            var_types.insert(vid, sv.ty.clone());
            var_places.insert(sv.name.clone(), pid);
        }

        // 3. ReqSteps -> HLN transitions with guard & update arcs.
        let mut milestone_sites: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        for m in &self.milestones {
            milestone_sites.entry(m.name.clone()).or_default();
        }
        let mut step_ids: BTreeMap<unipn::net::TransitionId, String> = BTreeMap::new();
        for (s_idx, step) in self.steps.iter().enumerate() {
            let r_idx = role_idx_of[&step.role];
            let src_pid = place_map[&(step.role.clone(), step.from_state.clone())];
            let dst_pid = place_map[&(step.role.clone(), step.to_state.clone())];

            let mut tr = HlnTransition::new(HlnTransitionKind::ReqStep);
            tr.scope = Some(step.role.clone());
            tr.func_id = Some(r_idx);
            tr.sid_idx = Some(s_idx);
            tr.sid = Some(step.id.clone());
            tr.milestone = step.milestone.clone();

            if let Some(ms) = &step.milestone {
                milestone_sites
                    .entry(ms.clone())
                    .or_default()
                    .push((r_idx, s_idx));
            }

            let tid = builder.add_transition(format!("{}::{}", step.role, step.id), tr);
            step_ids.insert(tid, step.id.clone());

            let guard = match &step.guard {
                Some(g) => parse_guard_bool_expr(g, &env)?,
                None => BoolExpr::True,
            };
            builder.add_guard_arc(src_pid, tid, guard);
            builder.add_update_arc(tid, dst_pid, None);

            for upd in &step.updates {
                let parsed = expr::parse(&upd.expr, &env).map_err(|e| e.message)?;
                let u_expr = to_unipn_expr(&parsed)?;
                let mut map = BTreeMap::new();
                map.insert(upd.target.clone(), u_expr);
                let v_pid = var_places[&upd.target];
                builder.add_update_arc(tid, v_pid, Some(map));
            }
        }

        let (net, initial) = builder.build();
        Ok(LoweredReqHln {
            net,
            initial,
            var_ids,
            var_types,
            terminal_places,
            milestone_sites,
            step_ids,
        })
    }
}

pub struct ReqGraph {
    pub lowered: LoweredReqHln,
    pub states: Vec<HlnState>,
    pub edges: Vec<(usize, usize, Option<String>, String)>,
    pub kripke_edges: Vec<(usize, usize, usize)>,
    pub deadlock_states: Vec<usize>,
    pub reached_milestones: BTreeSet<String>,
    pub truncated: bool,
}

fn state_is_terminal(state: &HlnState, terminal_places: &BTreeSet<PlaceId>) -> bool {
    for (&pid, toks) in &state.control_tokens {
        if !toks.is_empty() && !terminal_places.contains(&pid) {
            return false;
        }
    }
    true
}

fn vars_within_bounds(state: &HlnState, var_types: &BTreeMap<usize, BaseType>) -> bool {
    for (&vid, ty) in var_types {
        if let Some(hval) = state.vars.get(&vid) {
            let v = hln_to_value(hval);
            if !within_type(&v, ty) {
                return false;
            }
        }
    }
    true
}

/// Lower and finitely explore a [`ReqProgram`] up to `max_states`.
pub fn explore_req(
    req: &ReqProgram,
    max_states: usize,
) -> Result<ReqGraph, Vec<ReqDiagnostic>> {
    let diags = validate_req_program(req);
    if !diags.is_empty() {
        return Err(diags);
    }
    let lowered = req
        .lower_to_hln()
        .map_err(|e| vec![diag("R050", e, None)])?;

    let mut states = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut edges = Vec::new();
    let mut kripke_edges = Vec::new();
    let mut deadlock_states = Vec::new();
    let mut reached_milestones = BTreeSet::new();
    let mut truncated = false;

    seen.insert(state_key(&lowered.initial), 0);
    states.push(lowered.initial.clone());
    let mut queue = VecDeque::from([0usize]);

    while let Some(idx) = queue.pop_front() {
        let state = states[idx].clone();
        let succs: Vec<_> = lowered
            .net
            .enabled_colored(&state)
            .into_iter()
            .filter(|(_, _, next)| vars_within_bounds(next, &lowered.var_types))
            .collect();

        if succs.is_empty() {
            if !state_is_terminal(&state, &lowered.terminal_places) {
                deadlock_states.push(idx);
            }
            continue;
        }

        for (tid, _bound_tid, next) in succs {
            let key = state_key(&next);
            let target = if let Some(&existing) = seen.get(&key) {
                existing
            } else {
                if states.len() >= max_states {
                    truncated = true;
                    continue;
                }
                let next_idx = states.len();
                seen.insert(key, next_idx);
                states.push(next);
                queue.push_back(next_idx);
                next_idx
            };

            let ms = lowered
                .net
                .transition(tid)
                .and_then(|t| t.kind.milestone.clone());
            if let Some(m) = &ms {
                reached_milestones.insert(m.clone());
            }
            let step_id = lowered
                .step_ids
                .get(&tid)
                .cloned()
                .unwrap_or_else(|| format!("t{}", tid.index()));
            let edge_id = edges.len();
            edges.push((idx, target, ms, step_id));
            kripke_edges.push((idx, target, edge_id));
        }
    }

    Ok(ReqGraph {
        lowered,
        states,
        edges,
        kripke_edges,
        deadlock_states,
        reached_milestones,
        truncated,
    })
}

fn strip_default_prefix<'a>(req: &ReqProgram, name: &'a str) -> &'a str {
    if let Some((m, rest)) = name.split_once("::") {
        if m == "main" || m == req.name {
            return rest;
        }
    }
    name
}

fn eval_req_predicate(graph: &ReqGraph, req: &ReqProgram, state_idx: usize, pred: &PredicateSpec) -> Result<Option<bool>, String> {
    let state = &graph.states[state_idx];
    match pred {
        PredicateSpec::True => Ok(Some(true)),
        PredicateSpec::False => Ok(Some(false)),
        PredicateSpec::VarEq { resource, value } => {
            let key = strip_default_prefix(req, resource);
            let Some(&vid) = graph.lowered.var_ids.get(key) else {
                return Ok(None);
            };
            let ty = &graph.lowered.var_types[&vid];
            let target = from_json(value, ty)?;
            Ok(Some(
                state.vars.get(&vid).map(hln_to_value).as_ref() == Some(&target),
            ))
        }
        PredicateSpec::VarCmp {
            resource,
            op,
            value,
        } => {
            let key = strip_default_prefix(req, resource);
            let Some(&vid) = graph.lowered.var_ids.get(key) else {
                return Ok(None);
            };
            let ty = &graph.lowered.var_types[&vid];
            let target = from_json(value, ty)?;
            let cmp_op = match op.as_str() {
                "==" => CmpOp::Eq,
                "!=" => CmpOp::Ne,
                "<" => CmpOp::Lt,
                "<=" => CmpOp::Le,
                ">" => CmpOp::Gt,
                ">=" => CmpOp::Ge,
                other => return Err(format!("unknown comparison operator '{other}'")),
            };
            let actual = state
                .vars
                .get(&vid)
                .map(hln_to_value)
                .unwrap_or_else(|| default_value(ty));
            Ok(concir_sem::system::compare_values(cmp_op, &actual, &target))
        }
        PredicateSpec::MilestoneReached { milestone } => {
            let Some(sites) = graph.lowered.milestone_sites.get(milestone) else {
                return Err(format!("unknown milestone '{milestone}' in req_ir"));
            };
            Ok(Some(
                sites.iter().any(|site| state.reached.contains(site)),
            ))
        }
        PredicateSpec::Not { predicate } => {
            Ok(eval_req_predicate(graph, req, state_idx, predicate)?.map(|b| !b))
        }
        PredicateSpec::And { predicates } => {
            let mut all = true;
            for p in predicates {
                match eval_req_predicate(graph, req, state_idx, p)? {
                    Some(v) => all &= v,
                    None => return Ok(None),
                }
            }
            Ok(Some(all))
        }
        PredicateSpec::Or { predicates } => {
            let mut any = false;
            for p in predicates {
                match eval_req_predicate(graph, req, state_idx, p)? {
                    Some(v) => any |= v,
                    None => return Ok(None),
                }
            }
            Ok(Some(any))
        }
        // Implementation-level sync / function predicates do not exist in `N_req`.
        _ => Ok(None),
    }
}

fn pred_applicable_to_req(graph: &ReqGraph, req: &ReqProgram, pred: &PredicateSpec) -> Result<bool, String> {
    eval_req_predicate(graph, req, 0, pred).map(|opt| opt.is_some())
}

fn ctl_applicable(graph: &ReqGraph, req: &ReqProgram, f: &CtlFormula<PredicateSpec>) -> Result<bool, String> {
    match f {
        CtlFormula::True | CtlFormula::False => Ok(true),
        CtlFormula::Atom { atom } => pred_applicable_to_req(graph, req, atom),
        CtlFormula::Not { arg }
        | CtlFormula::Ex { arg }
        | CtlFormula::Ax { arg }
        | CtlFormula::Ef { arg }
        | CtlFormula::Af { arg }
        | CtlFormula::Eg { arg }
        | CtlFormula::Ag { arg } => ctl_applicable(graph, req, arg),
        CtlFormula::And { args } | CtlFormula::Or { args } => {
            for a in args {
                if !ctl_applicable(graph, req, a)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        CtlFormula::Implies { lhs, rhs }
        | CtlFormula::Eu { lhs, rhs }
        | CtlFormula::Au { lhs, rhs } => {
            Ok(ctl_applicable(graph, req, lhs)? && ctl_applicable(graph, req, rhs)?)
        }
    }
}

fn ltl_applicable(graph: &ReqGraph, req: &ReqProgram, f: &LtlFormula<PredicateSpec>) -> Result<bool, String> {
    match f {
        LtlFormula::True | LtlFormula::False => Ok(true),
        LtlFormula::Atom { atom } => pred_applicable_to_req(graph, req, atom),
        LtlFormula::Not { arg }
        | LtlFormula::Next { arg }
        | LtlFormula::Eventually { arg }
        | LtlFormula::Always { arg } => ltl_applicable(graph, req, arg),
        LtlFormula::And { args } | LtlFormula::Or { args } => {
            for a in args {
                if !ltl_applicable(graph, req, a)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        LtlFormula::Implies { lhs, rhs }
        | LtlFormula::Until { lhs, rhs }
        | LtlFormula::Release { lhs, rhs } => {
            Ok(ltl_applicable(graph, req, lhs)? && ltl_applicable(graph, req, rhs)?)
        }
    }
}

fn check_ctl_on_req(
    graph: &ReqGraph,
    req: &ReqProgram,
    id: &str,
    formula: &CtlFormula<PredicateSpec>,
    non_vacuous: bool,
    complete: bool,
) -> ReqPropertyResult {
    match ctl_applicable(graph, req, formula) {
        Err(e) => {
            return ReqPropertyResult {
                id: id.to_string(),
                outcome: "invalid".into(),
                detail: e,
                vacuous: false,
                vacuous_antecedents: Vec::new(),
            }
        }
        Ok(false) => {
            return ReqPropertyResult {
                id: id.to_string(),
                outcome: "skipped".into(),
                detail: "skipped on N_req (references implementation-only lock/function predicates)".into(),
                vacuous: false,
                vacuous_antecedents: Vec::new(),
            }
        }
        Ok(true) => {}
    }

    let res = check_ctl_graph(
        graph.states.len(),
        0,
        &graph.kripke_edges,
        formula,
        |s, a| eval_req_predicate(graph, req, s, a).ok().flatten().unwrap_or(false),
        PredicateSpec::explain,
    );
    if res.satisfied && non_vacuous && res.vacuous {
        return ReqPropertyResult {
            id: id.to_string(),
            outcome: if complete { "fail" } else { "unknown" }.into(),
            detail: format!(
                "CTL property is vacuously satisfied ({}): {}",
                explain_ctl(formula, PredicateSpec::explain),
                res.vacuous_antecedents.join("; ")
            ),
            vacuous: true,
            vacuous_antecedents: res.vacuous_antecedents,
        };
    }
    if res.satisfied {
        ReqPropertyResult {
            id: id.to_string(),
            outcome: if complete { "pass" } else { "unknown" }.into(),
            detail: explain_ctl(formula, PredicateSpec::explain),
            vacuous: res.vacuous,
            vacuous_antecedents: res.vacuous_antecedents,
        }
    } else {
        ReqPropertyResult {
            id: id.to_string(),
            outcome: "fail".into(),
            detail: format!(
                "CTL property violated on N_req: {}",
                explain_ctl(formula, PredicateSpec::explain)
            ),
            vacuous: false,
            vacuous_antecedents: Vec::new(),
        }
    }
}

fn check_ltl_on_req(
    graph: &ReqGraph,
    req: &ReqProgram,
    id: &str,
    formula: &LtlFormula<PredicateSpec>,
    fairness: &[FairnessConstraint<PredicateSpec>],
    non_vacuous: bool,
    complete: bool,
) -> ReqPropertyResult {
    match ltl_applicable(graph, req, formula) {
        Err(e) => {
            return ReqPropertyResult {
                id: id.to_string(),
                outcome: "invalid".into(),
                detail: e,
                vacuous: false,
                vacuous_antecedents: Vec::new(),
            }
        }
        Ok(false) => {
            return ReqPropertyResult {
                id: id.to_string(),
                outcome: "skipped".into(),
                detail: "skipped on N_req (references implementation-only lock/function predicates)".into(),
                vacuous: false,
                vacuous_antecedents: Vec::new(),
            }
        }
        Ok(true) => {}
    }

    for fc in fairness {
        let ok = match fc {
            FairnessConstraint::Justice { atom } => pred_applicable_to_req(graph, req, atom),
            FairnessConstraint::Compassion { enabled, taken } => Ok(
                pred_applicable_to_req(graph, req, enabled).unwrap_or(false)
                    && pred_applicable_to_req(graph, req, taken).unwrap_or(false),
            ),
        };
        if !ok.unwrap_or(false) {
            return ReqPropertyResult {
                id: id.to_string(),
                outcome: "skipped".into(),
                detail: "skipped on N_req (fairness references implementation-only predicates)".into(),
                vacuous: false,
                vacuous_antecedents: Vec::new(),
            };
        }
    }

    let res = check_ltl_graph(
        graph.states.len(),
        0,
        &graph.kripke_edges,
        formula,
        fairness,
        |s, a| eval_req_predicate(graph, req, s, a).ok().flatten().unwrap_or(false),
        PredicateSpec::explain,
    );
    if res.satisfied && non_vacuous && res.vacuous {
        return ReqPropertyResult {
            id: id.to_string(),
            outcome: if complete { "fail" } else { "unknown" }.into(),
            detail: format!(
                "LTL property is vacuously satisfied ({}): {}",
                explain_ltl(formula, fairness, PredicateSpec::explain),
                res.vacuous_antecedents.join("; ")
            ),
            vacuous: true,
            vacuous_antecedents: res.vacuous_antecedents,
        };
    }
    if res.satisfied {
        ReqPropertyResult {
            id: id.to_string(),
            outcome: if complete { "pass" } else { "unknown" }.into(),
            detail: explain_ltl(formula, fairness, PredicateSpec::explain),
            vacuous: res.vacuous,
            vacuous_antecedents: res.vacuous_antecedents,
        }
    } else {
        ReqPropertyResult {
            id: id.to_string(),
            outcome: "fail".into(),
            detail: format!(
                "LTL property violated on N_req: {}",
                explain_ltl(formula, fairness, PredicateSpec::explain)
            ),
            vacuous: false,
            vacuous_antecedents: Vec::new(),
        }
    }
}

/// Validate, lower to `UniPN::HlnNet`, explore, and check a [`ReqProgram`]
/// (deadlock-freedom of non-terminal states, non-dead milestones, and optional
/// contract properties applicable to `N_req`).
pub fn check_req_program(
    req: &ReqProgram,
    contract: Option<&ContractSpec>,
    max_states: usize,
) -> ReqCheckReport {
    let declared_milestones: Vec<String> = req.milestones.iter().map(|m| m.name.clone()).collect();
    let graph = match explore_req(req, max_states) {
        Ok(g) => g,
        Err(diagnostics) => {
            return ReqCheckReport {
                schema: REQ_CHECK_SCHEMA.to_string(),
                outcome: "invalid".to_string(),
                complete: true,
                states_explored: 0,
                transitions_explored: 0,
                deadlock_free: false,
                no_dead_milestones: false,
                declared_milestones,
                reachable_milestones: Vec::new(),
                missing_milestones: Vec::new(),
                deadlock_states: Vec::new(),
                diagnostics,
                properties: Vec::new(),
            };
        }
    };

    let complete = !graph.truncated;
    let deadlock_free = graph.deadlock_states.is_empty();
    let reachable_milestones: Vec<String> = graph.reached_milestones.iter().cloned().collect();
    let missing_milestones: Vec<String> = declared_milestones
        .iter()
        .filter(|m| !graph.reached_milestones.contains(*m))
        .cloned()
        .collect();
    let no_dead_milestones = missing_milestones.is_empty();

    let mut diagnostics = Vec::new();
    if !deadlock_free {
        diagnostics.push(diag(
            "R101",
            format!(
                "N_req has {} reachable non-terminal deadlock state(s): {:?}",
                graph.deadlock_states.len(),
                graph.deadlock_states
            ),
            None,
        ));
    }
    if !no_dead_milestones && complete {
        diagnostics.push(diag(
            "R102",
            format!(
                "N_req contains dead/unreachable milestone(s): [{}]",
                missing_milestones.join(", ")
            ),
            None,
        ));
    }

    let mut properties = Vec::new();
    let mut any_invalid_prop = false;
    let mut any_fail_prop = false;
    let mut any_unknown_prop = false;

    if let Some(c) = contract {
        for p in &c.properties {
            let prop_res = match p {
                PropertySpec::DeadlockFree { id, .. } => ReqPropertyResult {
                    id: id.clone(),
                    outcome: if !deadlock_free {
                        "fail"
                    } else if complete {
                        "pass"
                    } else {
                        "unknown"
                    }
                    .into(),
                    detail: "no reachable non-terminal deadlock state in N_req".into(),
                    vacuous: false,
                    vacuous_antecedents: Vec::new(),
                },
                PropertySpec::Safety { id, invariant, .. } => {
                    let f = CtlFormula::Ag {
                        arg: Box::new(CtlFormula::Atom {
                            atom: invariant.clone(),
                        }),
                    };
                    check_ctl_on_req(&graph, req, id, &f, false, complete)
                }
                PropertySpec::Unreachable { id, bad, .. } => {
                    let f = CtlFormula::Ag {
                        arg: Box::new(CtlFormula::Not {
                            arg: Box::new(CtlFormula::Atom { atom: bad.clone() }),
                        }),
                    };
                    check_ctl_on_req(&graph, req, id, &f, false, complete)
                }
                PropertySpec::Reachability { id, goal, .. } => {
                    let f = CtlFormula::Ef {
                        arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                    };
                    check_ctl_on_req(&graph, req, id, &f, false, complete)
                }
                PropertySpec::AlwaysReachable { id, goal, .. } => {
                    let f = CtlFormula::Ag {
                        arg: Box::new(CtlFormula::Ef {
                            arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                        }),
                    };
                    check_ctl_on_req(&graph, req, id, &f, false, complete)
                }
                PropertySpec::Ctl {
                    id,
                    formula,
                    non_vacuous,
                    ..
                } => check_ctl_on_req(&graph, req, id, formula, *non_vacuous, complete),
                PropertySpec::Ltl {
                    id,
                    formula,
                    fairness,
                    non_vacuous,
                    ..
                } => check_ltl_on_req(&graph, req, id, formula, fairness, *non_vacuous, complete),
                PropertySpec::Pattern {
                    id,
                    pattern,
                    non_vacuous,
                    ..
                } => match pattern {
                    ConstraintPattern::Always { cond } => check_ctl_on_req(
                        &graph,
                        req,
                        id,
                        &CtlFormula::Ag {
                            arg: Box::new(CtlFormula::Atom { atom: cond.clone() }),
                        },
                        *non_vacuous,
                        complete,
                    ),
                    ConstraintPattern::Exclude { preds } => {
                        let inner = if preds.len() == 1 {
                            CtlFormula::Not {
                                arg: Box::new(CtlFormula::Atom {
                                    atom: preds[0].clone(),
                                }),
                            }
                        } else {
                            CtlFormula::Not {
                                arg: Box::new(CtlFormula::And {
                                    args: preds
                                        .iter()
                                        .map(|a| CtlFormula::Atom { atom: a.clone() })
                                        .collect(),
                                }),
                            }
                        };
                        check_ctl_on_req(
                            &graph,
                            req,
                            id,
                            &CtlFormula::Ag {
                                arg: Box::new(inner),
                            },
                            false,
                            complete,
                        )
                    }
                    ConstraintPattern::CanReach { goal } => check_ctl_on_req(
                        &graph,
                        req,
                        id,
                        &CtlFormula::Ef {
                            arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                        },
                        false,
                        complete,
                    ),
                    ConstraintPattern::AlwaysCanReach { goal } => check_ctl_on_req(
                        &graph,
                        req,
                        id,
                        &CtlFormula::Ag {
                            arg: Box::new(CtlFormula::Ef {
                                arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                            }),
                        },
                        false,
                        complete,
                    ),
                    ConstraintPattern::Precedence { effect, cause } => {
                        let eff = LtlFormula::Atom {
                            atom: effect.clone(),
                        };
                        let cau = LtlFormula::Atom {
                            atom: cause.clone(),
                        };
                        let f = LtlFormula::Or {
                            args: vec![
                                LtlFormula::Until {
                                    lhs: Box::new(LtlFormula::Not {
                                        arg: Box::new(eff.clone()),
                                    }),
                                    rhs: Box::new(cau.clone()),
                                },
                                LtlFormula::Always {
                                    arg: Box::new(LtlFormula::Not {
                                        arg: Box::new(eff),
                                    }),
                                },
                            ],
                        };
                        check_ltl_on_req(&graph, req, id, &f, &[], *non_vacuous, complete)
                    }
                    ConstraintPattern::Triggers {
                        cause,
                        effect,
                        fairness,
                    } => {
                        let f = LtlFormula::Always {
                            arg: Box::new(LtlFormula::Implies {
                                lhs: Box::new(LtlFormula::Atom {
                                    atom: cause.clone(),
                                }),
                                rhs: Box::new(LtlFormula::Eventually {
                                    arg: Box::new(LtlFormula::Atom {
                                        atom: effect.clone(),
                                    }),
                                }),
                            }),
                        };
                        check_ltl_on_req(&graph, req, id, &f, fairness, *non_vacuous, complete)
                    }
                    ConstraintPattern::AtMost { .. } => ReqPropertyResult {
                        id: id.clone(),
                        outcome: "skipped".into(),
                        detail: "at_most pattern evaluated during design verification".into(),
                        vacuous: false,
                        vacuous_antecedents: Vec::new(),
                    },
                },
                PropertySpec::Health { id, .. } => ReqPropertyResult {
                    id: id.clone(),
                    outcome: "skipped".into(),
                    detail: "concurrency health suite applies to design/implementation net N_code".into(),
                    vacuous: false,
                    vacuous_antecedents: Vec::new(),
                },
            };

            match prop_res.outcome.as_str() {
                "invalid" => any_invalid_prop = true,
                "fail" => any_fail_prop = true,
                "unknown" => any_unknown_prop = true,
                _ => {}
            }
            properties.push(prop_res);
        }
    }

    let outcome = if any_invalid_prop {
        "invalid"
    } else if !deadlock_free || (complete && !no_dead_milestones) || any_fail_prop {
        "fail"
    } else if !complete || any_unknown_prop {
        "unknown"
    } else {
        "pass"
    };

    ReqCheckReport {
        schema: REQ_CHECK_SCHEMA.to_string(),
        outcome: outcome.to_string(),
        complete,
        states_explored: graph.states.len(),
        transitions_explored: graph.edges.len(),
        deadlock_free,
        no_dead_milestones,
        declared_milestones,
        reachable_milestones,
        missing_milestones,
        deadlock_states: graph.deadlock_states,
        diagnostics,
        properties,
    }
}
