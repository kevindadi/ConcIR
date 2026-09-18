//! Structured CIR patches with a strict version guard.
//!
//! A patch names a target module/function, records the original content hash,
//! lists structured changes, and keeps stable provenance. Application fails
//! loudly on an unknown target, a hash mismatch, or a conflicting/duplicate
//! change.

use serde::{Deserialize, Serialize};

use crate::ast::{Function, Op, Program};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatchChange {
    /// Swap two statements of a function body. The statements must be
    /// non-control, adjacent lock acquisitions, and must not be control
    /// targets (checked by the provider and re-checked here).
    SwapStatements { a: String, b: String },
    /// Delete a statement. Only used by explicit candidate files; the
    /// automatic enumerator never deletes statements. Required to be
    /// non-control and not a control target.
    DeleteStatement { sid: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRelation {
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CirPatch {
    pub id: String,
    pub module: String,
    pub function: String,
    /// Hex content hash of the target function before the patch.
    pub original_hash: String,
    pub changes: Vec<PatchChange>,
    #[serde(default)]
    pub provenance: Vec<SourceRelation>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatchError {
    UnknownModule(String),
    UnknownFunction(String),
    HashMismatch { expected: String, actual: String },
    UnknownSid(String),
    DuplicateChange(String),
    IllegalChange(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::UnknownModule(m) => write!(f, "unknown module '{m}'"),
            PatchError::UnknownFunction(fn_) => write!(f, "unknown function '{fn_}'"),
            PatchError::HashMismatch { expected, actual } => {
                write!(f, "hash mismatch: expected {expected}, found {actual}")
            }
            PatchError::UnknownSid(s) => write!(f, "unknown statement sid '{s}'"),
            PatchError::DuplicateChange(c) => write!(f, "duplicate change: {c}"),
            PatchError::IllegalChange(c) => write!(f, "illegal change: {c}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// FNV-1a 64 hex over the serialized target function.
pub fn function_hash(
    program: &Program,
    module: &str,
    function: &str,
) -> Result<String, PatchError> {
    let f = find_function(program, module, function)?;
    let json = serde_json::to_string(f).map_err(|e| PatchError::IllegalChange(e.to_string()))?;
    let mut h: u64 = 0xcbf29ce484222325;
    for b in json.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Ok(format!("{h:016x}"))
}

pub fn find_function<'a>(
    program: &'a Program,
    module: &str,
    function: &str,
) -> Result<&'a Function, PatchError> {
    let m = program
        .lookup_module(module)
        .ok_or_else(|| PatchError::UnknownModule(module.to_string()))?;
    m.functions
        .iter()
        .find(|f| f.name == function)
        .ok_or_else(|| PatchError::UnknownFunction(function.to_string()))
}

/// Apply a patch, returning the patched program and a textual diff.
pub fn apply(program: &Program, patch: &CirPatch) -> Result<(Program, String), PatchError> {
    let actual = function_hash(program, &patch.module, &patch.function)?;
    if actual != patch.original_hash {
        return Err(PatchError::HashMismatch {
            expected: patch.original_hash.clone(),
            actual,
        });
    }
    let mut patched = program.clone();
    let module = patched
        .modules
        .iter_mut()
        .find(|m| m.name == patch.module)
        .ok_or_else(|| PatchError::UnknownModule(patch.module.clone()))?;
    let f = module
        .functions
        .iter_mut()
        .find(|f| f.name == patch.function)
        .ok_or_else(|| PatchError::UnknownFunction(patch.function.clone()))?;

    let mut seen = std::collections::HashSet::new();
    let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
    for change in &patch.changes {
        let key = format!("{change:?}");
        if !seen.insert(key) {
            return Err(PatchError::DuplicateChange(format!("{change:?}")));
        }
        // A statement touched by two changes is a conflict, regardless of the
        // change kinds. This is typed, not a Debug-string coincidence.
        let ids: Vec<&String> = match change {
            PatchChange::SwapStatements { a, b } => vec![a, b],
            PatchChange::DeleteStatement { sid } => vec![sid],
        };
        for id in ids {
            if !touched.insert(id.clone()) {
                return Err(PatchError::DuplicateChange(format!(
                    "statement '{id}' is modified by more than one change"
                )));
            }
        }
        match change {
            PatchChange::SwapStatements { a, b } => apply_swap(f, a, b)?,
            PatchChange::DeleteStatement { sid } => apply_delete(f, sid)?,
        }
    }

    let diff = render_diff(&patch.function, &patch.changes);
    Ok((patched, diff))
}

fn apply_swap(f: &mut Function, a: &str, b: &str) -> Result<(), PatchError> {
    let ia = f
        .body
        .iter()
        .position(|s| &s.sid == a)
        .ok_or_else(|| PatchError::UnknownSid(a.to_string()))?;
    let ib = f
        .body
        .iter()
        .position(|s| &s.sid == b)
        .ok_or_else(|| PatchError::UnknownSid(b.to_string()))?;
    if ia == ib {
        return Err(PatchError::IllegalChange(
            "swap of a statement with itself".into(),
        ));
    }
    // Both must be lock acquisitions and adjacent, with no control target on
    // either sid. This keeps the swap semantics-preserving apart from order.
    let is_lock = |op: &Op| matches!(op, Op::MutexLock { .. });
    if !is_lock(&f.body[ia].op) || !is_lock(&f.body[ib].op) {
        return Err(PatchError::IllegalChange(
            "swap only applies to adjacent mutex_lock statements".into(),
        ));
    }
    let adjacent = (ia as i64 - ib as i64).abs() == 1;
    if !adjacent {
        return Err(PatchError::IllegalChange(
            "swap only applies to adjacent statements".into(),
        ));
    }
    if is_control_target(f, a) || is_control_target(f, b) {
        return Err(PatchError::IllegalChange(
            "cannot swap a statement that is a control-flow target".into(),
        ));
    }
    f.body.swap(ia, ib);
    Ok(())
}

pub fn is_control_target(f: &Function, sid: &str) -> bool {
    f.body.iter().any(|s| match &s.op {
        Op::Goto { target } => target == sid,
        Op::Branch {
            then, else_target, ..
        } => then == sid || else_target == sid,
        Op::Switch { cases, default, .. } => cases.values().any(|t| t == sid) || default == sid,
        Op::Select { branches, default } => {
            branches.iter().any(|b| &b.target == sid) || default.as_deref() == Some(sid)
        }
        _ => false,
    })
}

fn apply_delete(f: &mut Function, sid: &str) -> Result<(), PatchError> {
    let idx = f
        .body
        .iter()
        .position(|s| s.sid == sid)
        .ok_or_else(|| PatchError::UnknownSid(sid.to_string()))?;
    if f.body[idx].is_control() {
        return Err(PatchError::IllegalChange(
            "refusing to delete a control-transfer statement".into(),
        ));
    }
    if is_control_target(f, sid) {
        return Err(PatchError::IllegalChange(
            "cannot delete a statement that is a control-flow target".into(),
        ));
    }
    f.body.remove(idx);
    Ok(())
}

fn render_diff(function: &str, changes: &[PatchChange]) -> String {
    let lines: Vec<String> = changes
        .iter()
        .map(|c| match c {
            PatchChange::SwapStatements { a, b } => {
                format!("- {function}: swap {a} <-> {b}")
            }
            PatchChange::DeleteStatement { sid } => {
                format!("- {function}: delete {sid}")
            }
        })
        .collect();
    lines.join("\n")
}

/// Provider-independent permission check. Every candidate — automatic or from
/// a file — must pass this before it can be applied or verified.
pub fn check_allowed(
    scope: &crate::explore::contract::PatchScope,
    patch: &CirPatch,
) -> Result<(), PatchError> {
    if !scope.allows_module(&patch.module) {
        return Err(PatchError::IllegalChange(format!(
            "module '{}' is outside the allowed patch scope",
            patch.module
        )));
    }
    if !scope.allows_function(&patch.module, &patch.function) {
        return Err(PatchError::IllegalChange(format!(
            "function '{}::{}' is outside the allowed patch scope",
            patch.module, patch.function
        )));
    }
    for change in &patch.changes {
        match change {
            PatchChange::SwapStatements { .. } if !scope.allow_lock_reorder => {
                return Err(PatchError::IllegalChange(
                    "lock reordering is not allowed by the contract".into(),
                ))
            }
            PatchChange::DeleteStatement { .. } if !scope.allow_statement_delete => {
                return Err(PatchError::IllegalChange(
                    "statement deletion is not allowed by the contract".into(),
                ))
            }
            _ => {}
        }
    }
    Ok(())
}
