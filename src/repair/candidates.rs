//! Candidate providers. No LLM implementation exists here.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::Deserialize;

use crate::ast::{Op, Program};
use crate::explore::contract::{ContractSpec, PatchScope};

use super::patch::{function_hash, is_control_target, CirPatch, PatchChange, SourceRelation};

/// Context handed to a provider each round.
pub struct RepairContext<'a> {
    pub program: &'a Program,
    pub spec: &'a ContractSpec,
    pub round: usize,
}

pub trait CandidateProvider {
    fn name(&self) -> &str;
    fn next_candidate(&mut self, ctx: &RepairContext) -> Option<CirPatch>;
}

// ─────────────────────── deterministic lock-order enumerator ─────────

/// Enumerates swaps of adjacent, side-effect-free mutex acquisitions in the
/// functions in scope. This is the concrete precondition for the lock-order
/// repair: two adjacent `mutex_lock` statements with different resources and
/// no control-flow target on either sid.
pub struct LockOrderEnumerator {
    targets: Vec<(String, String, String, String)>,
    cursor: usize,
}

impl LockOrderEnumerator {
    pub fn new(program: &Program, scope: &PatchScope) -> Self {
        let mut targets = Vec::new();
        for m in &program.modules {
            for f in &m.functions {
                if !scope.allows_function(&f.name) {
                    continue;
                }
                for i in 0..f.body.len().saturating_sub(1) {
                    let a = &f.body[i];
                    let b = &f.body[i + 1];
                    let (ra, rb) = match (&a.op, &b.op) {
                        (Op::MutexLock { resource: ra }, Op::MutexLock { resource: rb }) => {
                            (ra, rb)
                        }
                        _ => continue,
                    };
                    if ra == rb {
                        continue;
                    }
                    if is_control_target(f, &a.sid) || is_control_target(f, &b.sid) {
                        continue;
                    }
                    targets.push((
                        m.name.clone(),
                        f.name.clone(),
                        a.sid.clone(),
                        b.sid.clone(),
                    ));
                }
            }
        }
        LockOrderEnumerator { targets, cursor: 0 }
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

impl CandidateProvider for LockOrderEnumerator {
    fn name(&self) -> &str {
        "lock-order-enumerator"
    }

    fn next_candidate(&mut self, ctx: &RepairContext) -> Option<CirPatch> {
        while self.cursor < self.targets.len() {
            let (module, function, a, b) = self.targets[self.cursor].clone();
            self.cursor += 1;
            if !ctx.spec.allowed_scope.allow_lock_reorder {
                return None;
            }
            let hash = function_hash(ctx.program, &module, &function).ok()?;
            return Some(CirPatch {
                id: format!("lockorder:{module}:{function}:{a}-{b}"),
                module,
                function,
                original_hash: hash,
                changes: vec![PatchChange::SwapStatements { a, b }],
                provenance: vec![SourceRelation {
                    description: "unify lock acquisition order for adjacent mutex locks"
                        .into(),
                }],
            });
        }
        None
    }
}

// ───────────────────────────── file provider ─────────────────────────

#[derive(Debug, Deserialize)]
struct FilePatchSpec {
    module: String,
    function: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    original_hash: Option<String>,
    changes: Vec<PatchChange>,
    #[serde(default)]
    provenance: Vec<SourceRelation>,
}

/// Reads a JSON array of patch specs from a file and yields them in order.
pub struct FileCandidateProvider {
    patches: VecDeque<CirPatch>,
    cursor: usize,
}

impl FileCandidateProvider {
    pub fn from_file(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::from_json(&text)
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let specs: Vec<FilePatchSpec> =
            serde_json::from_str(text).map_err(|e| format!("parse patch file: {e}"))?;
        let patches = specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| CirPatch {
                id: s.id.unwrap_or_else(|| format!("file[{i}]")),
                module: s.module,
                function: s.function,
                original_hash: s.original_hash.unwrap_or_default(),
                changes: s.changes,
                provenance: s.provenance,
            })
            .collect();
        Ok(FileCandidateProvider {
            patches,
            cursor: 0,
        })
    }
}

impl CandidateProvider for FileCandidateProvider {
    fn name(&self) -> &str {
        "file"
    }

    fn next_candidate(&mut self, ctx: &RepairContext) -> Option<CirPatch> {
        while self.cursor < self.patches.len() {
            let mut p = self.patches[self.cursor].clone();
            self.cursor += 1;
            if p.original_hash.is_empty() {
                p.original_hash = function_hash(ctx.program, &p.module, &p.function).ok()?;
            }
            return Some(p);
        }
        None
    }
}
