//! Contract-driven finite monitors.
//!
//! Historical facts (`completed_functions`) can otherwise grow without bound
//! and turn a finite control loop into an infinite state space. The verifier
//! derives the maximum interesting count of every function from the contract:
//! a plain `FunctionCompleted` only needs a boolean, `FunctionCompletedAtLeast`
//! saturates at `n`, and unobserved functions are not counted at all.

use std::collections::BTreeMap;

use crate::sem::ids::FunctionId;
use crate::sem::system::Predicate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Default saturation for functions not mentioned in `saturate`.
    pub default_max: usize,
    pub saturate: BTreeMap<FunctionId, usize>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self::unbounded()
    }
}

impl MonitorConfig {
    /// Count without bound (engine default when no contract is provided).
    pub fn unbounded() -> Self {
        MonitorConfig {
            default_max: usize::MAX,
            saturate: BTreeMap::new(),
        }
    }

    /// Count nothing unless the contract asks for it.
    pub fn finite(per_function: BTreeMap<FunctionId, usize>) -> Self {
        MonitorConfig {
            default_max: 0,
            saturate: per_function,
        }
    }

    pub fn max_for(&self, function: FunctionId) -> usize {
        self.saturate
            .get(&function)
            .copied()
            .unwrap_or(self.default_max)
    }

    /// Derive the saturation thresholds required by a set of predicates.
    pub fn from_predicates<'a>(predicates: impl IntoIterator<Item = &'a Predicate>) -> Self {
        let mut saturate: BTreeMap<FunctionId, usize> = BTreeMap::new();
        for p in predicates {
            collect(p, &mut saturate);
        }
        MonitorConfig::finite(saturate)
    }
}

fn collect(predicate: &Predicate, out: &mut BTreeMap<FunctionId, usize>) {
    match predicate {
        Predicate::FunctionCompleted { func } => {
            let e = out.entry(*func).or_insert(0);
            *e = (*e).max(1);
        }
        Predicate::FunctionCompletedAtLeast { func, n } => {
            let e = out.entry(*func).or_insert(0);
            *e = (*e).max(*n);
        }
        Predicate::Not(p) => collect(p, out),
        Predicate::And(ps) | Predicate::Or(ps) => {
            for p in ps {
                collect(p, out);
            }
        }
        _ => {}
    }
}
