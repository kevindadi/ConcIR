//! Verification contract: properties, preserved behaviour, assumptions, and
//! the allowed patch scope. A contract is built from data and is never
//! modified by a candidate patch.

use serde::{Deserialize, Serialize};
use unipn::analysis::obligations::BugKind;
use unipn::analysis::temporal::{
    explain_ctl, explain_ltl, CtlFormula, FairnessConstraint, LtlFormula,
};

use super::patterns::{CompiledPattern, ConstraintPattern};

use concir_ir::expr::CmpOp;
use concir_sem::ids::{FunctionId, ResourceId};
use concir_sem::program::{ResKind, SemProgram};
use concir_sem::system::Predicate;
use concir_sem::value::{from_json, Value};

use super::spec::*;

impl ContractSpec {
    pub fn resolve(&self, program: &SemProgram) -> Result<VerificationContract, ContractError> {
        validate_assumptions(&self.assumptions)?;
        validate_bounds(&self.bounds)?;
        let mut seen_ids = std::collections::HashSet::new();
        for p in &self.properties {
            let id = property_id(p);
            if id.trim().is_empty() {
                return Err(ContractError::Invalid(
                    "property id must not be empty".into(),
                ));
            }
            if !seen_ids.insert(id.to_string()) {
                return Err(ContractError::Invalid(format!(
                    "duplicate property id '{id}'"
                )));
            }
        }
        // Unqualified contract names resolve in the entry module's namespace,
        // which is stable under declaration/module reordering (never
        // `ModuleId(0)`).
        let default_module = program.entry_module();
        let mut properties = Vec::new();
        for p in &self.properties {
            let resolve = |e: &PredicateSpec| {
                e.resolve(program, default_module)
                    .map_err(ContractError::Invalid)
            };
            let (id, property) = match p {
                PropertySpec::Safety { id, invariant, .. } => (
                    id.clone(),
                    Property::Safety {
                        invariant: resolve(invariant)?,
                    },
                ),
                PropertySpec::DeadlockFree { id, .. } => (id.clone(), Property::DeadlockFree),
                PropertySpec::Reachability { id, goal, .. } => (
                    id.clone(),
                    Property::Reachability {
                        goal: resolve(goal)?,
                    },
                ),
                PropertySpec::AlwaysReachable { id, goal, .. } => (
                    id.clone(),
                    Property::AlwaysReachable {
                        goal: resolve(goal)?,
                    },
                ),
                PropertySpec::Unreachable { id, bad, .. } => {
                    (id.clone(), Property::Unreachable { bad: resolve(bad)? })
                }
                PropertySpec::Ctl {
                    id,
                    formula,
                    non_vacuous,
                    ..
                } => {
                    let resolved = resolve_ctl(formula, &resolve)?;
                    let explanation = explain_ctl(formula, PredicateSpec::explain);
                    (
                        id.clone(),
                        Property::Ctl {
                            formula: resolved,
                            non_vacuous: *non_vacuous,
                            explanation,
                        },
                    )
                }
                PropertySpec::Ltl {
                    id,
                    formula,
                    fairness,
                    non_vacuous,
                    ..
                } => {
                    let resolved = resolve_ltl(formula, &resolve)?;
                    let mut resolved_fairness = Vec::with_capacity(fairness.len());
                    for fc in fairness {
                        resolved_fairness.push(resolve_fairness(fc, &resolve)?);
                    }
                    let explanation = explain_ltl(formula, fairness, PredicateSpec::explain);
                    (
                        id.clone(),
                        Property::Ltl {
                            formula: resolved,
                            fairness: resolved_fairness,
                            non_vacuous: *non_vacuous,
                            explanation,
                        },
                    )
                }
                PropertySpec::Pattern {
                    id,
                    pattern,
                    non_vacuous,
                    ..
                } => {
                    let prop = match pattern.resolve(program, default_module, *non_vacuous)? {
                        CompiledPattern::Ctl {
                            formula,
                            non_vacuous,
                            explanation,
                        } => Property::Ctl {
                            formula,
                            non_vacuous,
                            explanation,
                        },
                        CompiledPattern::Ltl {
                            formula,
                            fairness,
                            non_vacuous,
                            witness_reachable,
                            witness_desc,
                            explanation,
                        } => Property::PatternLtl {
                            formula,
                            fairness,
                            non_vacuous,
                            witness_reachable,
                            witness_desc,
                            explanation,
                        },
                    };
                    (id.clone(), prop)
                }
                PropertySpec::Health { id, checks, .. } => {
                    let active = if checks.is_empty() {
                        BugKind::default_suite()
                    } else {
                        checks.clone()
                    };
                    let names: Vec<&str> = active.iter().map(|b| b.label()).collect();
                    (
                        id.clone(),
                        Property::Health {
                            checks: active,
                            explanation: format!(
                                "Check concurrency health obligations: [{}].",
                                names.join(", ")
                            ),
                        },
                    )
                }
            };
            properties.push(PropertySpecResolved { id, property });
        }
        let mut preserved = Vec::new();
        for p in &self.preserved {
            preserved.push(match p {
                PreservedSpec::Reachable {
                    description, goal, ..
                } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Reachable(
                        goal.resolve(program, default_module)
                            .map_err(ContractError::Invalid)?,
                    ),
                },
                PreservedSpec::Always {
                    description,
                    invariant,
                    ..
                } => PreservedBehavior {
                    description: description.clone(),
                    behavior: Preserved::Always(
                        invariant
                            .resolve(program, default_module)
                            .map_err(ContractError::Invalid)?,
                    ),
                },
            });
        }
        Ok(VerificationContract {
            name: self.name.clone(),
            properties,
            preserved,
            assumptions: self.assumptions.clone(),
            bounds: (&self.bounds).into(),
            allowed_scope: self.allowed_scope.clone(),
            source: self.clone(),
        })
    }
}

fn resolve_ctl<F>(
    f: &CtlFormula<PredicateSpec>,
    resolve: &F,
) -> Result<CtlFormula<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match f {
        CtlFormula::True => CtlFormula::True,
        CtlFormula::False => CtlFormula::False,
        CtlFormula::Atom { atom } => CtlFormula::Atom {
            atom: resolve(atom)?,
        },
        CtlFormula::Not { arg } => CtlFormula::Not {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::And { args } => CtlFormula::And {
            args: args
                .iter()
                .map(|a| resolve_ctl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        CtlFormula::Or { args } => CtlFormula::Or {
            args: args
                .iter()
                .map(|a| resolve_ctl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        CtlFormula::Implies { lhs, rhs } => CtlFormula::Implies {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
        CtlFormula::Ex { arg } => CtlFormula::Ex {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ax { arg } => CtlFormula::Ax {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ef { arg } => CtlFormula::Ef {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Af { arg } => CtlFormula::Af {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Eg { arg } => CtlFormula::Eg {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Ag { arg } => CtlFormula::Ag {
            arg: Box::new(resolve_ctl(arg, resolve)?),
        },
        CtlFormula::Eu { lhs, rhs } => CtlFormula::Eu {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
        CtlFormula::Au { lhs, rhs } => CtlFormula::Au {
            lhs: Box::new(resolve_ctl(lhs, resolve)?),
            rhs: Box::new(resolve_ctl(rhs, resolve)?),
        },
    })
}

fn resolve_ltl<F>(
    f: &LtlFormula<PredicateSpec>,
    resolve: &F,
) -> Result<LtlFormula<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match f {
        LtlFormula::True => LtlFormula::True,
        LtlFormula::False => LtlFormula::False,
        LtlFormula::Atom { atom } => LtlFormula::Atom {
            atom: resolve(atom)?,
        },
        LtlFormula::Not { arg } => LtlFormula::Not {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::And { args } => LtlFormula::And {
            args: args
                .iter()
                .map(|a| resolve_ltl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        LtlFormula::Or { args } => LtlFormula::Or {
            args: args
                .iter()
                .map(|a| resolve_ltl(a, resolve))
                .collect::<Result<Vec<_>, _>>()?,
        },
        LtlFormula::Implies { lhs, rhs } => LtlFormula::Implies {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
        LtlFormula::Next { arg } => LtlFormula::Next {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Eventually { arg } => LtlFormula::Eventually {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Always { arg } => LtlFormula::Always {
            arg: Box::new(resolve_ltl(arg, resolve)?),
        },
        LtlFormula::Until { lhs, rhs } => LtlFormula::Until {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
        LtlFormula::Release { lhs, rhs } => LtlFormula::Release {
            lhs: Box::new(resolve_ltl(lhs, resolve)?),
            rhs: Box::new(resolve_ltl(rhs, resolve)?),
        },
    })
}

fn resolve_fairness<F>(
    fc: &FairnessConstraint<PredicateSpec>,
    resolve: &F,
) -> Result<FairnessConstraint<Predicate>, ContractError>
where
    F: Fn(&PredicateSpec) -> Result<Predicate, ContractError>,
{
    Ok(match fc {
        FairnessConstraint::Justice { atom } => FairnessConstraint::Justice {
            atom: resolve(atom)?,
        },
        FairnessConstraint::Compassion { enabled, taken } => FairnessConstraint::Compassion {
            enabled: resolve(enabled)?,
            taken: resolve(taken)?,
        },
    })
}

pub(super) fn property_id(p: &PropertySpec) -> &str {
    match p {
        PropertySpec::Safety { id, .. }
        | PropertySpec::DeadlockFree { id, .. }
        | PropertySpec::Reachability { id, .. }
        | PropertySpec::AlwaysReachable { id, .. }
        | PropertySpec::Unreachable { id, .. }
        | PropertySpec::Ctl { id, .. }
        | PropertySpec::Ltl { id, .. }
        | PropertySpec::Pattern { id, .. }
        | PropertySpec::Health { id, .. } => id,
    }
}

fn validate_assumptions(a: &Assumptions) -> Result<(), ContractError> {
    if !a.sequential_consistency {
        return Err(ContractError::Unsupported(
            "sequential_consistency = false is not modeled".into(),
        ));
    }
    if !a.no_spurious_wakeups {
        return Err(ContractError::Unsupported(
            "no_spurious_wakeups = false is not modeled".into(),
        ));
    }
    Ok(())
}

fn validate_bounds(b: &BoundsSpec) -> Result<(), ContractError> {
    for (name, v) in [
        ("max_threads", b.max_threads),
        ("max_frames_per_thread", b.max_frames_per_thread),
        ("max_states", b.max_states),
        ("max_depth", b.max_depth),
        ("max_boundary_events", b.max_boundary_events),
    ] {
        if v == 0 {
            return Err(ContractError::Invalid(format!("{name} must be >= 1")));
        }
    }
    Ok(())
}

fn resolve_resource(
    program: &SemProgram,
    default_module: concir_sem::ids::ModuleId,
    name: &str,
) -> Result<ResourceId, String> {
    program
        .resolve_resource(default_module, name)
        .ok_or_else(|| format!("unknown resource '{name}'"))
}

fn expect_kind(
    program: &SemProgram,
    resource: ResourceId,
    name: &str,
    allowed: &[ResKind],
) -> Result<(), String> {
    let r = program.resource(resource);
    if allowed.contains(&r.kind) {
        Ok(())
    } else {
        Err(format!(
            "resource '{name}' has an incompatible kind for this predicate"
        ))
    }
}

fn resolve_function(
    program: &SemProgram,
    default_module: concir_sem::ids::ModuleId,
    name: &str,
) -> Result<FunctionId, String> {
    program
        .resolve_function(default_module, name)
        .ok_or_else(|| format!("unknown function '{name}'"))
}

fn resolve_value(
    program: &SemProgram,
    resource: ResourceId,
    json: &serde_json::Value,
) -> Result<Value, String> {
    let r = program.resource(resource);
    if !matches!(r.kind, ResKind::Var | ResKind::Atomic) {
        return Err(format!("resource '{}' is not a Var/Atomic", r.name));
    }
    let ty = r.ty.as_ref().ok_or("resource has no value type")?;
    from_json(json, ty)
}

fn parse_op(op: &str) -> Result<CmpOp, String> {
    Ok(match op {
        "==" => CmpOp::Eq,
        "!=" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        "<=" => CmpOp::Le,
        ">" => CmpOp::Gt,
        ">=" => CmpOp::Ge,
        other => return Err(format!("unknown comparison operator '{other}'")),
    })
}


impl PredicateSpec {
    pub fn resolve(
        &self,
        program: &SemProgram,
        default_module: concir_sem::ids::ModuleId,
    ) -> Result<Predicate, String> {
        Ok(match self {
            PredicateSpec::True => Predicate::True,
            PredicateSpec::False => Predicate::False,
            PredicateSpec::VarEq { resource, value } => {
                let rid = resolve_resource(program, default_module, resource)?;
                let v = resolve_value(program, rid, value)?;
                Predicate::VarEq {
                    resource: rid,
                    value: v,
                }
            }
            PredicateSpec::VarCmp {
                resource,
                op,
                value,
            } => {
                let rid = resolve_resource(program, default_module, resource)?;
                let v = resolve_value(program, rid, value)?;
                Predicate::VarCmp {
                    resource: rid,
                    op: parse_op(op)?,
                    value: v,
                }
            }
            PredicateSpec::FunctionCompleted { function } => Predicate::FunctionCompleted {
                func: resolve_function(program, default_module, function)?,
            },
            PredicateSpec::FunctionCompletedAtLeast { function, n } => {
                Predicate::FunctionCompletedAtLeast {
                    func: resolve_function(program, default_module, function)?,
                    n: *n,
                }
            }
            PredicateSpec::ScopeCompleted { function, sid } => {
                let f = resolve_function(program, default_module, function)?;
                let idx = program
                    .function(f)
                    .stmt_index(sid)
                    .ok_or_else(|| format!("function '{function}' has no statement '{sid}'"))?;
                Predicate::ScopeCompleted { func: f, sid: idx }
            }
            PredicateSpec::StatementReached { function, sid } => {
                let f = resolve_function(program, default_module, function)?;
                let idx = program
                    .function(f)
                    .stmt_index(sid)
                    .ok_or_else(|| format!("function '{function}' has no statement '{sid}'"))?;
                Predicate::StatementReached { func: f, sid: idx }
            }
            PredicateSpec::MilestoneReached { milestone } => {
                let sites = program.milestone_sites(milestone);
                if sites.is_empty() {
                    return Err(format!(
                        "unknown milestone '{milestone}' (no statement carries this milestone)"
                    ));
                }
                Predicate::MilestoneReached {
                    milestone: milestone.clone(),
                    sites,
                }
            }
            PredicateSpec::MutexFree { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Mutex, ResKind::RwLock])?;
                Predicate::MutexFree(rid)
            }
            PredicateSpec::MutexHeld { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Mutex, ResKind::RwLock])?;
                Predicate::MutexHeld(rid)
            }
            PredicateSpec::HoldsAll {
                function,
                resources,
            } => {
                let fid = resolve_function(program, default_module, function)?;
                let mut rids = Vec::new();
                for name in resources {
                    let rid = resolve_resource(program, default_module, name)?;
                    expect_kind(
                        program,
                        rid,
                        name,
                        &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                    )?;
                    rids.push(rid);
                }
                Predicate::HoldsAll {
                    func: fid,
                    resources: rids,
                }
            }
            PredicateSpec::MutexExclusive { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(
                    program,
                    rid,
                    resource,
                    &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                )?;
                Predicate::MutexExclusive(rid)
            }
            PredicateSpec::NeverHoldsAll {
                function,
                resources,
            } => {
                let fid = resolve_function(program, default_module, function)?;
                let mut rids = Vec::new();
                for name in resources {
                    let rid = resolve_resource(program, default_module, name)?;
                    expect_kind(
                        program,
                        rid,
                        name,
                        &[ResKind::Mutex, ResKind::RwLock, ResKind::Semaphore],
                    )?;
                    rids.push(rid);
                }
                Predicate::NeverHoldsAll {
                    func: fid,
                    resources: rids,
                }
            }
            PredicateSpec::ChannelEmpty { resource } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Channel])?;
                Predicate::ChannelEmpty(rid)
            }
            PredicateSpec::ChannelAtLeast { resource, len } => {
                let rid = resolve_resource(program, default_module, resource)?;
                expect_kind(program, rid, resource, &[ResKind::Channel])?;
                Predicate::ChannelAtLeast {
                    resource: rid,
                    len: *len,
                }
            }
            PredicateSpec::Not { predicate } => {
                Predicate::not(predicate.resolve(program, default_module)?)
            }
            PredicateSpec::And { predicates } => Predicate::and(
                predicates
                    .iter()
                    .map(|p| p.resolve(program, default_module))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            PredicateSpec::Or { predicates } => Predicate::or(
                predicates
                    .iter()
                    .map(|p| p.resolve(program, default_module))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }
}

