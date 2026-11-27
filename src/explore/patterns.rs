//! Reusable high-level temporal constraint pattern DSL (`ConstraintPattern`).
//!
//! Compiles domain-neutral concurrency requirement patterns into UniPN CTL/LTL
//! formulas (with optional witness obligations for non-vacuity) and produces
//! deterministic natural-language explanations.

use serde::{Deserialize, Serialize};
use unipn::analysis::temporal::{explain_ctl, explain_ltl, CtlFormula, FairnessConstraint, LtlFormula};

use super::contract::{ContractError, PredicateSpec};
use crate::sem::program::SemProgram;
use crate::sem::system::Predicate;

/// Reusable concurrency requirement patterns that compile to CTL / Fair-LTL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pattern", rename_all = "snake_case")]
pub enum ConstraintPattern {
    /// Safety invariant `AG(cond)`: `cond` holds in every reachable state.
    Always { cond: PredicateSpec },
    /// Mutual exclusion `AG(!(p1 && p2 && ...))`: listed predicates cannot all
    /// hold simultaneously in any reachable state.
    Exclude { preds: Vec<PredicateSpec> },
    /// Bounded concurrency: at most `max` of `conds` hold at the same time.
    AtMost {
        conds: Vec<PredicateSpec>,
        max: usize,
    },
    /// Weak precedence: `effect` cannot occur unless `cause` has already
    /// occurred (or holds simultaneously): `(!effect) W cause`.
    Precedence {
        effect: PredicateSpec,
        cause: PredicateSpec,
    },
    /// Fair response `G(cause -> F effect)` under justice/compassion fairness.
    Triggers {
        cause: PredicateSpec,
        effect: PredicateSpec,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fairness: Vec<FairnessConstraint<PredicateSpec>>,
    },
    /// Existential reachability `EF(goal)`.
    CanReach { goal: PredicateSpec },
    /// Persistent recoverability `AG EF(goal)`.
    AlwaysCanReach { goal: PredicateSpec },
}

/// Resolved executable representation produced by [`ConstraintPattern::resolve`].
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledPattern {
    Ctl {
        formula: CtlFormula<Predicate>,
        non_vacuous: bool,
        explanation: String,
    },
    Ltl {
        formula: LtlFormula<Predicate>,
        fairness: Vec<FairnessConstraint<Predicate>>,
        non_vacuous: bool,
        /// Optional non-vacuity witness (`EF(effect)` for `Precedence`).
        witness_reachable: Option<Predicate>,
        witness_desc: Option<String>,
        explanation: String,
    },
}

impl ConstraintPattern {
    /// Deterministic natural-language explanation for NL back-translation.
    pub fn explain(&self) -> String {
        match self {
            ConstraintPattern::Always { cond } => {
                format!("Pattern(always): in every reachable state, {}.", cond.explain())
            }
            ConstraintPattern::Exclude { preds } => {
                let parts: Vec<String> = preds.iter().map(PredicateSpec::explain).collect();
                format!(
                    "Pattern(exclude): [{}] can never all hold simultaneously.",
                    parts.join(", ")
                )
            }
            ConstraintPattern::AtMost { conds, max } => {
                let parts: Vec<String> = conds.iter().map(PredicateSpec::explain).collect();
                format!(
                    "Pattern(at_most {max}): at most {max} of [{}] hold simultaneously.",
                    parts.join(", ")
                )
            }
            ConstraintPattern::Precedence { effect, cause } => format!(
                "Pattern(precedence): '{}' cannot hold before '{}' has occurred.",
                effect.explain(),
                cause.explain()
            ),
            ConstraintPattern::Triggers {
                cause,
                effect,
                fairness,
            } => {
                let sym = LtlFormula::Always {
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
                format!(
                    "Pattern(triggers): {}",
                    explain_ltl(&sym, fairness, PredicateSpec::explain)
                )
            }
            ConstraintPattern::CanReach { goal } => {
                let sym = CtlFormula::Ef {
                    arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                };
                format!("Pattern(can_reach): {}", explain_ctl(&sym, PredicateSpec::explain))
            }
            ConstraintPattern::AlwaysCanReach { goal } => {
                let sym = CtlFormula::Ag {
                    arg: Box::new(CtlFormula::Ef {
                        arg: Box::new(CtlFormula::Atom { atom: goal.clone() }),
                    }),
                };
                format!(
                    "Pattern(always_can_reach): {}",
                    explain_ctl(&sym, PredicateSpec::explain)
                )
            }
        }
    }

    /// Resolve symbolic predicates against `program` and compile into CTL/Fair-LTL.
    pub fn resolve(
        &self,
        program: &SemProgram,
        default_module: crate::sem::ids::ModuleId,
        non_vacuous: bool,
    ) -> Result<CompiledPattern, ContractError> {
        let explanation = self.explain();
        let res = |ps: &PredicateSpec| ps.resolve(program, default_module).map_err(ContractError::Invalid);
        match self {
            ConstraintPattern::Always { cond } => {
                let formula = CtlFormula::Ag {
                    arg: Box::new(CtlFormula::Atom { atom: res(cond)? }),
                };
                Ok(CompiledPattern::Ctl {
                    formula,
                    non_vacuous,
                    explanation,
                })
            }
            ConstraintPattern::Exclude { preds } => {
                if preds.is_empty() {
                    return Err(ContractError::Invalid(
                        "pattern 'exclude' requires at least one predicate".into(),
                    ));
                }
                let inner = if preds.len() == 1 {
                    CtlFormula::Not {
                        arg: Box::new(CtlFormula::Atom {
                            atom: res(&preds[0])?,
                        }),
                    }
                } else {
                    let mut args = Vec::with_capacity(preds.len());
                    for p in preds {
                        args.push(CtlFormula::Atom { atom: res(p)? });
                    }
                    CtlFormula::Not {
                        arg: Box::new(CtlFormula::And { args }),
                    }
                };
                Ok(CompiledPattern::Ctl {
                    formula: CtlFormula::Ag {
                        arg: Box::new(inner),
                    },
                    non_vacuous: false,
                    explanation,
                })
            }
            ConstraintPattern::AtMost { conds, max } => {
                let mut resolved = Vec::with_capacity(conds.len());
                for c in conds {
                    resolved.push(res(c)?);
                }
                let inner = if *max >= resolved.len() {
                    CtlFormula::True
                } else if *max == 0 {
                    let args = resolved
                        .into_iter()
                        .map(|a| CtlFormula::Not {
                            arg: Box::new(CtlFormula::Atom { atom: a }),
                        })
                        .collect();
                    CtlFormula::And { args }
                } else {
                    let mut bad_subsets = Vec::new();
                    let k = *max + 1;
                    let mut cur = Vec::with_capacity(k);
                    combine_indices(0, resolved.len(), k, &mut cur, &mut |picked| {
                        let args = picked
                            .iter()
                            .map(|&idx| CtlFormula::Atom {
                                atom: resolved[idx].clone(),
                            })
                            .collect();
                        bad_subsets.push(CtlFormula::Not {
                            arg: Box::new(CtlFormula::And { args }),
                        });
                    });
                    CtlFormula::And { args: bad_subsets }
                };
                Ok(CompiledPattern::Ctl {
                    formula: CtlFormula::Ag {
                        arg: Box::new(inner),
                    },
                    non_vacuous: false,
                    explanation,
                })
            }
            ConstraintPattern::Precedence { effect, cause } => {
                let eff = res(effect)?;
                let cau = res(cause)?;
                let not_eff = LtlFormula::Not {
                    arg: Box::new(LtlFormula::Atom { atom: eff.clone() }),
                };
                // Weak until: ((!effect) U cause) || G(!effect)
                let formula = LtlFormula::Or {
                    args: vec![
                        LtlFormula::Until {
                            lhs: Box::new(not_eff.clone()),
                            rhs: Box::new(LtlFormula::Atom { atom: cau }),
                        },
                        LtlFormula::Always {
                            arg: Box::new(not_eff),
                        },
                    ],
                };
                Ok(CompiledPattern::Ltl {
                    formula,
                    fairness: Vec::new(),
                    non_vacuous,
                    witness_reachable: non_vacuous.then_some(eff),
                    witness_desc: non_vacuous.then(|| effect.explain()),
                    explanation,
                })
            }
            ConstraintPattern::Triggers {
                cause,
                effect,
                fairness,
            } => {
                let cau = res(cause)?;
                let eff = res(effect)?;
                let mut resolved_fairness = Vec::with_capacity(fairness.len());
                for fc in fairness {
                    resolved_fairness.push(match fc {
                        FairnessConstraint::Justice { atom } => FairnessConstraint::Justice {
                            atom: res(atom)?,
                        },
                        FairnessConstraint::Compassion { enabled, taken } => {
                            FairnessConstraint::Compassion {
                                enabled: res(enabled)?,
                                taken: res(taken)?,
                            }
                        }
                    });
                }
                let formula = LtlFormula::Always {
                    arg: Box::new(LtlFormula::Implies {
                        lhs: Box::new(LtlFormula::Atom { atom: cau }),
                        rhs: Box::new(LtlFormula::Eventually {
                            arg: Box::new(LtlFormula::Atom { atom: eff }),
                        }),
                    }),
                };
                Ok(CompiledPattern::Ltl {
                    formula,
                    fairness: resolved_fairness,
                    non_vacuous,
                    witness_reachable: None,
                    witness_desc: None,
                    explanation,
                })
            }
            ConstraintPattern::CanReach { goal } => {
                let formula = CtlFormula::Ef {
                    arg: Box::new(CtlFormula::Atom { atom: res(goal)? }),
                };
                Ok(CompiledPattern::Ctl {
                    formula,
                    non_vacuous: false,
                    explanation,
                })
            }
            ConstraintPattern::AlwaysCanReach { goal } => {
                let formula = CtlFormula::Ag {
                    arg: Box::new(CtlFormula::Ef {
                        arg: Box::new(CtlFormula::Atom { atom: res(goal)? }),
                    }),
                };
                Ok(CompiledPattern::Ctl {
                    formula,
                    non_vacuous: false,
                    explanation,
                })
            }
        }
    }
}

fn combine_indices(
    start: usize,
    n: usize,
    k: usize,
    cur: &mut Vec<usize>,
    cb: &mut impl FnMut(&[usize]),
) {
    if cur.len() == k {
        cb(cur);
        return;
    }
    for i in start..n {
        cur.push(i);
        combine_indices(i + 1, n, k, cur, cb);
        cur.pop();
    }
}
