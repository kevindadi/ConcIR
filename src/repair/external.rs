//! External single-patch evaluation and replay.
//!
//! This module provides the minimal protocol for a *submitted* patch (for
//! example, one proposed by an LLM) to be evaluated against a frozen model and
//! contract, and later replayed offline. It deliberately reuses the existing
//! patch legality, static validation, supportability, contract re-binding and
//! verification implementations; it does **not** run the internal candidate
//! enumerator.
//!
//! Two capabilities:
//!
//! * [`build_context`] exports a repair context (fingerprints, allowed scope,
//!   root verification/diagnostics, functions with statement ids and the Rust
//!   `function_hash`). It never contains a solved patch.
//! * [`evaluate_patch`] applies and fully verifies exactly one submitted
//!   adjacent `mutex_lock` swap against the frozen context. Acceptance requires
//!   a complete `PASS` of *all* properties and preserved behaviour.
//!
//! The evaluated result is a versioned [`ExternalPatchArtifact`] that
//! [`replay_external_patch`] can re-apply and re-verify from scratch.

use serde::{Deserialize, Serialize};

use crate::ast::{Op, Program};
use crate::explore::contract::{ContractError, ContractSpec, PatchScope};
use crate::explore::{verify_program, EngineKind, VerificationReport};
use crate::sem::outcome::Outcome;
use crate::validate;

use super::patch::{self, function_hash, CirPatch, PatchChange, SourceRelation};
use super::search::{program_fingerprint, reports_match, AppliedEdit, ReplayResult};
use super::RepairOutcome;

pub const CONTEXT_SCHEMA: &str = "concir-repair-context-v1";
pub const CANDIDATE_SCHEMA: &str = "concir-external-patch-candidate-v1";
pub const ARTIFACT_SCHEMA: &str = "concir-external-patch-artifact-v1";

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockSite {
    pub sid: String,
    pub resource: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionContext {
    pub module: String,
    pub function: String,
    /// Rust-computed FNV hash of the function, identical to the patch guard.
    pub original_hash: String,
    pub locks: Vec<LockSite>,
    /// Statement ids that are control-flow targets (not swappable).
    pub control_targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairContextArtifact {
    pub schema_version: String,
    pub source: super::search::SourceIdentity,
    pub model_fingerprint: String,
    pub contract_fingerprint: String,
    pub context_fingerprint: String,
    pub allowed_scope: PatchScope,
    pub input_program: Program,
    pub frozen_contract: ContractSpec,
    pub root_report: VerificationReport,
    pub functions: Vec<FunctionContext>,
    pub reproduce: String,
}

/// The submitted patch exactly as the provider sent it (no Rust-added fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmittedPatch {
    pub module: String,
    pub function: String,
    #[serde(default)]
    pub original_hash: String,
    pub changes: Vec<PatchChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalPatchCandidate {
    pub schema_version: String,
    pub context_fingerprint: String,
    pub patch: SubmittedPatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalPatchArtifact {
    pub schema_version: String,
    pub source: super::search::SourceIdentity,
    pub context_fingerprint: String,
    pub model_fingerprint: String,
    pub contract_fingerprint: String,
    pub allowed_scope: PatchScope,
    pub input_program: Program,
    pub frozen_contract: ContractSpec,
    pub root_report: VerificationReport,
    pub submitted: ExternalPatchCandidate,
    pub patch: CirPatch,
    pub patch_chain: Vec<AppliedEdit>,
    pub patched_program: Option<Program>,
    pub patched_program_fingerprint: Option<String>,
    pub static_valid: bool,
    pub supported: bool,
    pub verification: Option<VerificationReport>,
    pub accepted: bool,
    /// `accepted` | `rejected`.
    pub status: String,
    pub reject_reason: Option<RejectReason>,
    pub reproduce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectReason {
    pub code: String,
    pub detail: String,
}

fn context_fingerprint(ctx: &RepairContextArtifact) -> String {
    let mut c = ctx.clone();
    c.context_fingerprint = String::new();
    fnv(&serde_json::to_string(&c).unwrap_or_default())
}

fn function_contexts(program: &Program) -> Vec<FunctionContext> {
    let mut out = Vec::new();
    for module in &program.modules {
        for function in &module.functions {
            let original_hash = function_hash(program, &module.name, &function.name)
                .unwrap_or_else(|_| String::new());
            let mut locks = Vec::new();
            let mut control_targets = Vec::new();
            for stmt in &function.body {
                if let Op::MutexLock { resource } = &stmt.op {
                    locks.push(LockSite {
                        sid: stmt.sid.clone(),
                        resource: resource.clone(),
                    });
                }
                if patch::is_control_target(function, &stmt.sid) {
                    control_targets.push(stmt.sid.clone());
                }
            }
            out.push(FunctionContext {
                module: module.name.clone(),
                function: function.name.clone(),
                original_hash,
                locks,
                control_targets,
            });
        }
    }
    out
}

/// Export the repair context for a frozen model + contract.
pub fn build_context(program: &Program, spec: &ContractSpec) -> RepairContextArtifact {
    let root_report = verify_program(program, spec, EngineKind::Petri);
    let mut ctx = RepairContextArtifact {
        schema_version: CONTEXT_SCHEMA.into(),
        source: super::search::source_identity(),
        model_fingerprint: program_fingerprint(program),
        contract_fingerprint: root_report.contract_fingerprint.clone(),
        context_fingerprint: String::new(),
        allowed_scope: spec.allowed_scope.clone(),
        input_program: program.clone(),
        frozen_contract: spec.clone(),
        root_report,
        functions: function_contexts(program),
        reproduce: "concir-backend repair-context <model.json> <contract.json>".into(),
    };
    ctx.context_fingerprint = context_fingerprint(&ctx);
    ctx
}

fn ok_status(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Pass => "accepted",
        _ => "rejected",
    }
}

fn outcome_to_repair(outcome: Outcome) -> RepairOutcome {
    match outcome {
        Outcome::Pass => RepairOutcome::Repaired,
        Outcome::Fail => RepairOutcome::NoAcceptableCandidate,
        Outcome::Unknown => RepairOutcome::AnalysisUnknown,
        Outcome::Invalid => RepairOutcome::Invalid,
        Outcome::Unsupported => RepairOutcome::Unsupported,
    }
}

fn rejected(
    ctx: &RepairContextArtifact,
    candidate: &ExternalPatchCandidate,
    patch: &CirPatch,
    code: &str,
    detail: String,
    static_valid: bool,
    supported: bool,
    verification: Option<VerificationReport>,
) -> ExternalPatchArtifact {
    ExternalPatchArtifact {
        schema_version: ARTIFACT_SCHEMA.into(),
        source: super::search::source_identity(),
        context_fingerprint: ctx.context_fingerprint.clone(),
        model_fingerprint: ctx.model_fingerprint.clone(),
        contract_fingerprint: ctx.contract_fingerprint.clone(),
        allowed_scope: ctx.allowed_scope.clone(),
        input_program: ctx.input_program.clone(),
        frozen_contract: ctx.frozen_contract.clone(),
        root_report: ctx.root_report.clone(),
        submitted: candidate.clone(),
        patch: patch.clone(),
        patch_chain: Vec::new(),
        patched_program: None,
        patched_program_fingerprint: None,
        static_valid,
        supported,
        verification,
        accepted: false,
        status: "rejected".into(),
        reject_reason: Some(RejectReason { code: code.into(), detail }),
        reproduce: "concir-backend evaluate-patch <context.json> <candidate.json>".into(),
    }
}

/// Evaluate exactly one submitted patch against the frozen context.
///
/// Returns `Err` only for a protocol/parse failure (no artifact). A well-formed
/// candidate always yields an artifact whose `status` is `accepted` or
/// `rejected` with a structured reason.
pub fn evaluate_patch(
    context_json: &str,
    candidate_json: &str,
) -> Result<ExternalPatchArtifact, String> {
    let ctx: RepairContextArtifact =
        serde_json::from_str(context_json).map_err(|e| format!("context parse: {e}"))?;
    if ctx.schema_version != CONTEXT_SCHEMA {
        return Err(format!("unknown context schema '{}'", ctx.schema_version));
    }
    let candidate: ExternalPatchCandidate =
        serde_json::from_str(candidate_json).map_err(|e| format!("candidate parse: {e}"))?;
    if candidate.schema_version != CANDIDATE_SCHEMA {
        return Err(format!(
            "unknown candidate schema '{}'",
            candidate.schema_version
        ));
    }

    // Binding: the context fingerprint must match the frozen context, and the
    // context itself must be self-consistent with its embedded program/contract.
    if ctx.context_fingerprint != context_fingerprint(&ctx) {
        return Err("context fingerprint is not self-consistent".into());
    }
    if candidate.context_fingerprint != ctx.context_fingerprint {
        let stub = CirPatch {
            id: "external".into(),
            module: candidate.patch.module.clone(),
            function: candidate.patch.function.clone(),
            original_hash: candidate.patch.original_hash.clone(),
            changes: candidate.patch.changes.clone(),
            provenance: vec![],
        };
        return Ok(rejected(
            &ctx, &candidate, &stub, "context_mismatch",
            format!(
                "candidate context_fingerprint {} does not match context {}",
                candidate.context_fingerprint, ctx.context_fingerprint
            ),
            false, false, None,
        ));
    }

    let patch = CirPatch {
        id: "external".into(),
        module: candidate.patch.module.clone(),
        function: candidate.patch.function.clone(),
        original_hash: candidate.patch.original_hash.clone(),
        changes: candidate.patch.changes.clone(),
        provenance: vec![SourceRelation {
            description: "external single patch".into(),
        }],
    };

    if patch.original_hash.is_empty() {
        return Ok(rejected(&ctx, &candidate, &patch, "missing_original_hash",
            "the submitted patch must carry the target function's original_hash".into(),
            false, false, None));
    }

    // Restricted shape: exactly one adjacent mutex_lock swap, different
    // resources. Enforced here, not only in the prompt.
    if patch.changes.len() != 1 {
        return Ok(rejected(&ctx, &candidate, &patch, "patch_shape",
            format!("exactly one change is allowed, got {}", patch.changes.len()),
            false, false, None));
    }
    match &patch.changes[0] {
        PatchChange::DeleteStatement { .. } => {
            return Ok(rejected(&ctx, &candidate, &patch, "patch_shape",
                "statement deletion is not allowed by the external patch protocol".into(),
                false, false, None));
        }
        PatchChange::SwapStatements { a, b } => {
            if a == b {
                return Ok(rejected(&ctx, &candidate, &patch, "patch_shape",
                    "swap targets must differ".into(), false, false, None));
            }
            let target = patch::find_function(&ctx.input_program, &patch.module, &patch.function);
            let target = match target {
                Ok(f) => f,
                Err(e) => {
                    return Ok(rejected(&ctx, &candidate, &patch, "unknown_target",
                        e.to_string(), false, false, None));
                }
            };
            let res = |sid: &str| -> Option<String> {
                target.body.iter().find(|s| s.sid == sid).and_then(|s| match &s.op {
                    Op::MutexLock { resource } => Some(resource.clone()),
                    _ => None,
                })
            };
            match (res(a), res(b)) {
                (Some(ra), Some(rb)) => {
                    if ra == rb {
                        return Ok(rejected(&ctx, &candidate, &patch, "same_resource",
                            format!("swap targets the same resource '{ra}'"),
                            false, false, None));
                    }
                }
                _ => {
                    return Ok(rejected(&ctx, &candidate, &patch, "unknown_sid",
                        format!("swap sids '{a}'/'{b}' are not both mutex_lock statements in {}::{}",
                                patch.module, patch.function),
                        false, false, None));
                }
            }
        }
    }

    if let Err(e) = patch::check_allowed(&ctx.allowed_scope, &patch) {
        return Ok(rejected(&ctx, &candidate, &patch, "scope_violation", e.to_string(),
            false, false, None));
    }

    let (patched, _diff) = match patch::apply(&ctx.input_program, &patch) {
        Ok(v) => v,
        Err(e) => {
            return Ok(rejected(&ctx, &candidate, &patch, "apply_error", e.to_string(),
                false, false, None));
        }
    };

    let static_report = validate::validate(&patched);
    if !static_report.valid {
        let n = static_report
            .diagnostics
            .iter()
            .filter(|d| d.severity == crate::diagnostic::Severity::Error)
            .count();
        return Ok(rejected(&ctx, &candidate, &patch, "static_invalid",
            format!("CIR static validation failed with {n} error(s)"), false, false, None));
    }

    let sem = match crate::sem::program::lower(&patched) {
        Ok(s) => s,
        Err(e) => {
            return Ok(rejected(&ctx, &candidate, &patch, "lower_error", e.to_string(),
                true, false, None));
        }
    };
    if !sem.unsupported().is_empty() {
        let names: Vec<String> = sem.unsupported().iter().map(|u| u.construct.clone()).collect();
        return Ok(rejected(&ctx, &candidate, &patch, "unsupported",
            format!("unsupported constructs: {}", names.join(", ")), true, false, None));
    }
    if let Err(e) = ctx.frozen_contract.resolve(&sem) {
        let detail = match e {
            ContractError::Unsupported(m) => format!("contract unsupported after patch: {m}"),
            ContractError::Invalid(m) => format!("contract cannot be re-bound after patch: {m}"),
        };
        return Ok(rejected(&ctx, &candidate, &patch, "contract_error", detail,
            true, true, None));
    }

    let report = verify_program(&patched, &ctx.frozen_contract, EngineKind::Petri);
    let accepted = report.outcome == Outcome::Pass;
    let patch_chain = vec![AppliedEdit {
        module: patch.module.clone(),
        function: patch.function.clone(),
        changes: patch.changes.clone(),
        provenance: patch.provenance.clone(),
        original_function_hash: patch.original_hash.clone(),
        parent_fingerprint: ctx.model_fingerprint.clone(),
        program_fingerprint: program_fingerprint(&patched),
    }];
    Ok(ExternalPatchArtifact {
        schema_version: ARTIFACT_SCHEMA.into(),
        source: super::search::source_identity(),
        context_fingerprint: ctx.context_fingerprint.clone(),
        model_fingerprint: ctx.model_fingerprint.clone(),
        contract_fingerprint: ctx.contract_fingerprint.clone(),
        allowed_scope: ctx.allowed_scope.clone(),
        input_program: ctx.input_program.clone(),
        frozen_contract: ctx.frozen_contract.clone(),
        root_report: ctx.root_report.clone(),
        submitted: candidate.clone(),
        patch: patch.clone(),
        patch_chain,
        patched_program: Some(patched.clone()),
        patched_program_fingerprint: Some(program_fingerprint(&patched)),
        static_valid: true,
        supported: true,
        verification: Some(report.clone()),
        accepted,
        status: ok_status(report.outcome).into(),
        reject_reason: if accepted {
            None
        } else {
            Some(RejectReason {
                code: match report.outcome {
                    Outcome::Fail => "verification_fail",
                    Outcome::Unknown => "verification_unknown",
                    Outcome::Invalid => "verification_invalid",
                    Outcome::Unsupported => "verification_unsupported",
                    Outcome::Pass => unreachable!(),
                }
                .into(),
                detail: format!(
                    "full contract verification outcome {:?} (complete={})",
                    report.outcome, report.complete
                ),
            })
        },
        reproduce: "concir-backend replay <artifact.json>".into(),
    })
}

/// Re-apply and re-verify an external patch artifact from its embedded inputs.
/// Rejects any tampering with the model, contract, patch or verification report.
pub fn replay_external_patch(artifact_json: &str) -> Result<ReplayResult, String> {
    let artifact: ExternalPatchArtifact =
        serde_json::from_str(artifact_json).map_err(|e| format!("artifact parse: {e}"))?;
    if artifact.schema_version != ARTIFACT_SCHEMA {
        return Err(format!("unknown artifact schema '{}'", artifact.schema_version));
    }

    // Recompute the context from the embedded inputs and check the binding.
    let ctx = build_context(&artifact.input_program, &artifact.frozen_contract);
    if ctx.context_fingerprint != artifact.context_fingerprint {
        return Err("context fingerprint does not match the embedded model/contract".into());
    }
    if ctx.model_fingerprint != artifact.model_fingerprint {
        return Err("model fingerprint mismatch".into());
    }
    if ctx.contract_fingerprint != artifact.contract_fingerprint {
        return Err("contract fingerprint mismatch".into());
    }

    // The stored root report must equal a fresh root verification.
    reports_match(&ctx.root_report, &artifact.root_report, "external root report")?;

    // Re-apply and verify the stored patch.
    let patch = artifact.patch.clone();
    if patch.changes.len() != 1 {
        return Err("stored patch must have exactly one change".into());
    }
    patch::check_allowed(&artifact.frozen_contract.allowed_scope, &patch)
        .map_err(|e| format!("stored patch is not allowed: {e}"))?;
    let (patched, _diff) = patch::apply(&artifact.input_program, &patch)
        .map_err(|e| format!("stored patch does not apply: {e}"))?;
    let static_report = validate::validate(&patched);
    if !static_report.valid {
        return Err("stored patched program fails static validation".into());
    }
    let sem = crate::sem::program::lower(&patched)
        .map_err(|e| format!("stored patched program cannot be lowered: {e}"))?;
    if !sem.unsupported().is_empty() {
        return Err("stored patched program has unsupported constructs".into());
    }
    artifact
        .frozen_contract
        .resolve(&sem)
        .map_err(|e| format!("stored patched program breaks the contract binding: {e}"))?;
    let report = verify_program(&patched, &artifact.frozen_contract, EngineKind::Petri);

    let stored = artifact
        .verification
        .as_ref()
        .ok_or_else(|| "artifact has no verification report".to_string())?;
    reports_match(&report, stored, "external verification report")?;

    if artifact.accepted != (report.outcome == Outcome::Pass) {
        return Err("artifact accepted flag contradicts the verification outcome".into());
    }
    if artifact.status != ok_status(report.outcome) {
        return Err("artifact status contradicts the verification outcome".into());
    }

    // The chain must describe exactly this single edit.
    if artifact.patch_chain.len() != 1 {
        return Err("artifact patch_chain must contain exactly one edit".into());
    }
    let edit = &artifact.patch_chain[0];
    let child_fp = program_fingerprint(&patched);
    if edit.module != patch.module
        || edit.function != patch.function
        || edit.changes != patch.changes
        || edit.original_function_hash != patch.original_hash
        || edit.parent_fingerprint != artifact.model_fingerprint
        || edit.program_fingerprint != child_fp
    {
        return Err("artifact patch_chain does not match the stored patch".into());
    }
    match &artifact.patched_program_fingerprint {
        Some(fp) if fp == &child_fp => {}
        _ => return Err("artifact patched program fingerprint mismatch".into()),
    }

    let accepted_ok = report.outcome == Outcome::Pass;
    Ok(ReplayResult {
        nodes: 1,
        input_outcome: ctx.root_report.outcome,
        accepted_ok: Some(accepted_ok),
        accepted_node: if accepted_ok { Some(0) } else { None },
        chain_len: 1,
        outcome: outcome_to_repair(report.outcome),
    })
}
