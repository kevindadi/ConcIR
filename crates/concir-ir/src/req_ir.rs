//! Requirement Net IR schema types (`concir-req-ir/1`, `concir-req-check/1`).

use serde::{Deserialize, Serialize};

use crate::ast::{BaseType, ModeledUpdate};

pub const REQ_IR_SCHEMA: &str = "concir-req-ir/1";
pub const REQ_CHECK_SCHEMA: &str = "concir-req-check/1";

fn default_req_ir_schema() -> String {
    REQ_IR_SCHEMA.to_string()
}

fn default_instances() -> usize {
    1
}

/// One concurrent domain role (actor) in `N_req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub name: String,
    #[serde(default = "default_instances")]
    pub instances: usize,
    pub init_state: String,
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub terminal_states: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
}

/// One abstract, bounded state variable observable by both `N_req` and contracts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateVarSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: BaseType,
    #[serde(default)]
    pub init: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
}

/// One observable business milestone anchored between `N_req` and `N_code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
}

/// One abstract step in `N_req`, optionally emitting a `milestone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqStep {
    pub id: String,
    pub role: String,
    pub from_state: String,
    pub to_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(default)]
    pub updates: Vec<ModeledUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub req: Vec<String>,
}

/// Top-level abstract requirement net (`concir-req-ir/1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReqProgram {
    #[serde(default = "default_req_ir_schema")]
    pub schema: String,
    pub name: String,
    #[serde(default)]
    pub roles: Vec<RoleSpec>,
    #[serde(default)]
    pub state_vars: Vec<StateVarSpec>,
    #[serde(default)]
    pub milestones: Vec<MilestoneSpec>,
    #[serde(default)]
    pub steps: Vec<ReqStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqPropertyResult {
    pub id: String,
    pub outcome: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vacuous: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vacuous_antecedents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqCheckReport {
    pub schema: String,
    pub outcome: String,
    pub complete: bool,
    pub states_explored: usize,
    pub transitions_explored: usize,
    pub deadlock_free: bool,
    pub no_dead_milestones: bool,
    pub declared_milestones: Vec<String>,
    pub reachable_milestones: Vec<String>,
    pub missing_milestones: Vec<String>,
    pub deadlock_states: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ReqDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<ReqPropertyResult>,
}

