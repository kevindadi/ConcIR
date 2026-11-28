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
    pub fn explain(&self) -> ContractExplanation {
        let properties = self
            .properties
            .iter()
            .map(|p| match p {
                PropertySpec::Safety { id, invariant, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "safety".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("In every reachable state, {}.", invariant.explain()),
                },
                PropertySpec::DeadlockFree { id, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "deadlock_free".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: "No reachable state is a global deadlock.".to_string(),
                },
                PropertySpec::Reachability { id, goal, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "reachability".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("There exists a reachable state where {}.", goal.explain()),
                },
                PropertySpec::AlwaysReachable { id, goal, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "always_reachable".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!(
                        "From every reachable state, it remains possible to reach a state where {}.",
                        goal.explain()
                    ),
                },
                PropertySpec::Unreachable { id, bad, req } => PropertyExplanation {
                    id: id.clone(),
                    kind: "unreachable".to_string(),
                    req: req.clone(),
                    non_vacuous: false,
                    explanation: format!("No reachable state satisfies {}.", bad.explain()),
                },
                PropertySpec::Ctl {
                    id,
                    formula,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "ctl".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: explain_ctl(formula, PredicateSpec::explain),
                },
                PropertySpec::Ltl {
                    id,
                    formula,
                    fairness,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "ltl".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: explain_ltl(formula, fairness, PredicateSpec::explain),
                },
                PropertySpec::Pattern {
                    id,
                    pattern,
                    non_vacuous,
                    req,
                } => PropertyExplanation {
                    id: id.clone(),
                    kind: "pattern".to_string(),
                    req: req.clone(),
                    non_vacuous: *non_vacuous,
                    explanation: pattern.explain(),
                },
                PropertySpec::Health { id, checks, req } => {
                    let active = if checks.is_empty() {
                        BugKind::default_suite()
                    } else {
                        checks.clone()
                    };
                    let names: Vec<&str> = active.iter().map(|b| b.label()).collect();
                    PropertyExplanation {
                        id: id.clone(),
                        kind: "health".to_string(),
                        req: req.clone(),
                        non_vacuous: false,
                        explanation: format!(
                            "Verify concurrency health obligations: [{}].",
                            names.join(", ")
                        ),
                    }
                }
            })
            .collect();

        let preserved = self
            .preserved
            .iter()
            .map(|p| match p {
                PreservedSpec::Reachable {
                    description,
                    goal,
                    req,
                } => PreservedExplanation {
                    description: description.clone(),
                    req: req.clone(),
                    explanation: format!(
                        "Behavior '{}' must remain reachable ({}).",
                        description,
                        goal.explain()
                    ),
                },
                PreservedSpec::Always {
                    description,
                    invariant,
                    req,
                } => PreservedExplanation {
                    description: description.clone(),
                    req: req.clone(),
                    explanation: format!(
                        "Invariant '{}' must always hold ({}).",
                        description,
                        invariant.explain()
                    ),
                },
            })
            .collect();

        ContractExplanation {
            schema: CONTRACT_EXPLANATION_SCHEMA.to_string(),
            name: self.name.clone(),
            properties,
            preserved,
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyExplanation {
    pub id: String,
    pub kind: String,
    pub req: Vec<String>,
    pub non_vacuous: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedExplanation {
    pub description: String,
    pub req: Vec<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractExplanation {
    pub schema: String,
    pub name: String,
    pub properties: Vec<PropertyExplanation>,
    pub preserved: Vec<PreservedExplanation>,
}


impl PredicateSpec {
    pub fn explain(&self) -> String {
        match self {
            PredicateSpec::True => "true".to_string(),
            PredicateSpec::False => "false".to_string(),
            PredicateSpec::VarEq { resource, value } => format!("{resource} == {value}"),
            PredicateSpec::VarCmp {
                resource,
                op,
                value,
            } => format!("{resource} {op} {value}"),
            PredicateSpec::FunctionCompleted { function } => {
                format!("function '{function}' has completed")
            }
            PredicateSpec::FunctionCompletedAtLeast { function, n } => {
                format!("function '{function}' has completed at least {n} time(s)")
            }
            PredicateSpec::ScopeCompleted { function, sid } => {
                format!("scope at '{function}::{sid}' has completed")
            }
            PredicateSpec::StatementReached { function, sid } => {
                format!("statement '{function}::{sid}' is reached")
            }
            PredicateSpec::MilestoneReached { milestone } => {
                format!("milestone '{milestone}' has been reached")
            }
            PredicateSpec::MutexFree { resource } => format!("lock '{resource}' is free"),
            PredicateSpec::MutexHeld { resource } => format!("lock '{resource}' is held"),
            PredicateSpec::HoldsAll {
                function,
                resources,
            } => format!(
                "a thread in '{function}' holds all of [{}]",
                resources.join(", ")
            ),
            PredicateSpec::MutexExclusive { resource } => {
                format!("lock '{resource}' maintains mutual exclusion")
            }
            PredicateSpec::NeverHoldsAll {
                function,
                resources,
            } => format!(
                "no thread in '{function}' simultaneously holds all of [{}]",
                resources.join(", ")
            ),
            PredicateSpec::ChannelEmpty { resource } => format!("channel '{resource}' is empty"),
            PredicateSpec::ChannelAtLeast { resource, len } => {
                format!("channel '{resource}' has at least {len} buffered item(s)")
            }
            PredicateSpec::Not { predicate } => format!("not ({})", predicate.explain()),
            PredicateSpec::And { predicates } => {
                if predicates.is_empty() {
                    "true".to_string()
                } else {
                    let parts: Vec<String> = predicates.iter().map(PredicateSpec::explain).collect();
                    format!("({})", parts.join(" and "))
                }
            }
            PredicateSpec::Or { predicates } => {
                if predicates.is_empty() {
                    "false".to_string()
                } else {
                    let parts: Vec<String> = predicates.iter().map(PredicateSpec::explain).collect();
                    format!("({})", parts.join(" or "))
                }
            }
        }
    }

}
