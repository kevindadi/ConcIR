//! Rust Single Source of Truth (SSoT) for Stage 0 contract sanity validation,
//! formula schema checks, resource-kind compatibility, clause verifiability,
//! entity grounding, and `non_vacuous: true` normalization (`concir-contract-validation/1`).

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONTRACT_VALIDATION_SCHEMA: &str = "concir-contract-validation/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanityIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clause_id: Option<String>,
}

impl SanityIssue {
    fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        property_id: Option<&str>,
        clause_id: Option<&str>,
    ) -> Self {
        Self {
            severity: "error".into(),
            code: code.into(),
            message: message.into(),
            property_id: property_id.map(str::to_string),
            clause_id: clause_id.map(str::to_string),
        }
    }

    fn with_severity(
        severity: &str,
        code: impl Into<String>,
        message: impl Into<String>,
        property_id: Option<&str>,
        clause_id: Option<&str>,
    ) -> Self {
        Self {
            severity: severity.to_string(),
            code: code.into(),
            message: message.into(),
            property_id: property_id.map(str::to_string),
            clause_id: clause_id.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanityReport {
    pub schema: String,
    pub ok: bool,
    pub checked_properties: usize,
    pub checked_preserved: usize,
    pub issues: Vec<SanityIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_contract: Option<Value>,
}

fn is_valid_property_kind(k: &str) -> bool {
    matches!(
        k,
        "safety"
            | "deadlock_free"
            | "reachability"
            | "always_reachable"
            | "unreachable"
            | "ctl"
            | "ltl"
            | "pattern"
            | "health"
    )
}

fn is_valid_pattern_kind(k: &str) -> bool {
    matches!(
        k,
        "always" | "exclude" | "at_most" | "precedence" | "triggers" | "can_reach" | "always_can_reach"
    )
}

fn is_valid_health_check(k: &str) -> bool {
    matches!(
        k,
        "deadlock"
            | "partial_deadlock"
            | "lost_wakeup"
            | "wake_starvation"
            | "acquire_starvation"
            | "resource_leak"
            | "double_lock"
            | "data_race"
    )
}

fn is_valid_predicate_kind(k: &str) -> bool {
    matches!(
        k,
        "true"
            | "false"
            | "var_eq"
            | "var_cmp"
            | "function_completed"
            | "function_completed_at_least"
            | "scope_completed"
            | "statement_reached"
            | "milestone_reached"
            | "mutex_free"
            | "mutex_held"
            | "holds_all"
            | "mutex_exclusive"
            | "never_holds_all"
            | "channel_empty"
            | "channel_at_least"
            | "not"
            | "and"
            | "or"
    )
}

fn is_valid_ctl_op(op: &str) -> bool {
    matches!(
        op,
        "true"
            | "false"
            | "atom"
            | "not"
            | "and"
            | "or"
            | "implies"
            | "ex"
            | "ax"
            | "ef"
            | "af"
            | "eg"
            | "ag"
            | "eu"
            | "au"
    )
}

fn is_valid_ltl_op(op: &str) -> bool {
    matches!(
        op,
        "true"
            | "false"
            | "atom"
            | "not"
            | "and"
            | "or"
            | "implies"
            | "next"
            | "eventually"
            | "always"
            | "until"
            | "release"
    )
}

fn is_valid_cmp_op(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=")
}

pub fn formula_contains_implication_value(formula: &Value) -> bool {
    let Some(obj) = formula.as_object() else {
        return false;
    };
    let op = obj
        .get("op")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if op == "implies" {
        return true;
    }
    if obj.get("arg").is_some_and(formula_contains_implication_value)
        || obj.get("lhs").is_some_and(formula_contains_implication_value)
        || obj.get("rhs").is_some_and(formula_contains_implication_value)
    {
        return true;
    }
    if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
        if args.iter().any(formula_contains_implication_value) {
            return true;
        }
    }
    false
}

pub fn ensure_implication_non_vacuous_value(contract: &Value) -> Value {
    let mut out = contract.clone();
    if let Some(props) = out.get_mut("properties").and_then(|v| v.as_array_mut()) {
        for prop in props {
            let Some(obj) = prop.as_object_mut() else {
                continue;
            };
            let kind = obj
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if matches!(kind.as_str(), "ctl" | "ltl") && !obj.contains_key("non_vacuous") {
                if obj.get("formula").is_some_and(formula_contains_implication_value) {
                    obj.insert("non_vacuous".into(), Value::Bool(true));
                }
            } else if kind == "pattern" && !obj.contains_key("non_vacuous") {
                let pat = obj
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if matches!(pat.as_str(), "triggers" | "precedence") {
                    obj.insert("non_vacuous".into(), Value::Bool(true));
                }
            }
        }
    }
    out
}

fn collect_predicate_refs(
    pred: Option<&Value>,
    functions: &mut BTreeSet<String>,
    resources: &mut BTreeSet<String>,
    milestones: Option<&mut BTreeSet<String>>,
    errors: &mut Vec<String>,
) {
    let Some(pred) = pred else {
        errors.push("predicate must be a JSON object, got NoneType".into());
        return;
    };
    let Some(obj) = pred.as_object() else {
        errors.push("predicate must be a JSON object".into());
        return;
    };
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !is_valid_predicate_kind(&kind) {
        errors.push(format!("unknown predicate kind '{kind}'"));
        return;
    }
    match kind.as_str() {
        "var_eq" | "var_cmp" | "mutex_free" | "mutex_held" | "mutex_exclusive"
        | "channel_empty" | "channel_at_least" => {
            match obj.get("resource").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(r) => {
                    resources.insert(r.to_string());
                }
                None => errors.push(format!("predicate '{kind}' missing 'resource'")),
            }
            if kind == "var_cmp" {
                let op = obj.get("op").and_then(|v| v.as_str()).unwrap_or("");
                if !is_valid_cmp_op(op) {
                    errors.push(format!("var_cmp has invalid op '{op}'"));
                }
            }
        }
        "function_completed"
        | "function_completed_at_least"
        | "scope_completed"
        | "statement_reached" => {
            match obj.get("function").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(f) => {
                    functions.insert(f.to_string());
                }
                None => errors.push(format!("predicate '{kind}' missing 'function'")),
            }
        }
        "milestone_reached" => {
            let ms = obj
                .get("milestone")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if ms.is_empty() {
                errors.push("predicate 'milestone_reached' missing non-empty 'milestone'".into());
            } else if let Some(ms_set) = milestones {
                ms_set.insert(ms.to_string());
            }
        }
        "holds_all" | "never_holds_all" => {
            match obj.get("function").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                Some(f) => {
                    functions.insert(f.to_string());
                }
                None => errors.push(format!("predicate '{kind}' missing 'function'")),
            }
            if let Some(arr) = obj.get("resources").and_then(|v| v.as_array()) {
                for r in arr {
                    if let Some(s) = r.as_str() {
                        resources.insert(s.to_string());
                    }
                }
            }
        }
        "not" => {
            collect_predicate_refs(obj.get("predicate"), functions, resources, milestones, errors);
        }
        "and" | "or" => {
            let Some(subs) = obj.get("predicates").and_then(|v| v.as_array()) else {
                errors.push(format!("predicate '{kind}' requires list 'predicates'"));
                return;
            };
            let mut ms_ref = milestones;
            for sub in subs {
                collect_predicate_refs(
                    Some(sub),
                    functions,
                    resources,
                    ms_ref.as_deref_mut(),
                    errors,
                );
            }
        }
        _ => {}
    }
}

fn validate_temporal_tree(
    node: Option<&Value>,
    logic: &str,
    functions: &mut BTreeSet<String>,
    resources: &mut BTreeSet<String>,
    mut milestones: Option<&mut BTreeSet<String>>,
    errors: &mut Vec<String>,
) {
    let Some(node) = node.and_then(|v| v.as_object()) else {
        errors.push(format!("{} formula node must be a JSON object", logic.to_ascii_uppercase()));
        return;
    };
    let op = node
        .get("op")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let valid = if logic == "ctl" {
        is_valid_ctl_op(&op)
    } else {
        is_valid_ltl_op(&op)
    };
    if !valid {
        errors.push(format!("unknown {} operator '{op}'", logic.to_ascii_uppercase()));
        return;
    }
    match op.as_str() {
        "true" | "false" => {}
        "atom" => collect_predicate_refs(
            node.get("atom"),
            functions,
            resources,
            milestones.as_deref_mut(),
            errors,
        ),
        "not" | "ex" | "ax" | "ef" | "af" | "eg" | "ag" | "next" | "eventually" | "always" => {
            validate_temporal_tree(node.get("arg"), logic, functions, resources, milestones, errors);
        }
        "implies" | "eu" | "au" | "until" | "release" => {
            validate_temporal_tree(
                node.get("lhs"),
                logic,
                functions,
                resources,
                milestones.as_deref_mut(),
                errors,
            );
            validate_temporal_tree(node.get("rhs"), logic, functions, resources, milestones, errors);
        }
        "and" | "or" => {
            let Some(args) = node.get("args").and_then(|v| v.as_array()).filter(|a| !a.is_empty())
            else {
                errors.push(format!(
                    "{} '{op}' requires non-empty list 'args'",
                    logic.to_ascii_uppercase()
                ));
                return;
            };
            for sub in args {
                validate_temporal_tree(
                    Some(sub),
                    logic,
                    functions,
                    resources,
                    milestones.as_deref_mut(),
                    errors,
                );
            }
        }
        _ => {}
    }
}

fn lookup_resource_kind<'a>(res: &str, map: &'a HashMap<String, String>) -> &'a str {
    let short = res.rsplit("::").next().unwrap_or(res);
    map.get(res)
        .or_else(|| map.get(short))
        .map(String::as_str)
        .unwrap_or("")
}

fn check_predicate_resource_compat(
    pred: Option<&Value>,
    res_kind_map: &HashMap<String, String>,
    property_id: &str,
    issues: &mut Vec<SanityIssue>,
) {
    if res_kind_map.is_empty() {
        return;
    }
    let Some(obj) = pred.and_then(|v| v.as_object()) else {
        return;
    };
    let pkind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match pkind.as_str() {
        "mutex_free" | "mutex_held" | "mutex_exclusive" => {
            let res = obj.get("resource").and_then(|v| v.as_str()).unwrap_or("");
            let dk = lookup_resource_kind(res, res_kind_map);
            if !dk.is_empty() && dk != "mutex" {
                issues.push(SanityIssue::error(
                    "incompatible_resource_predicate",
                    format!("predicate '{pkind}' requires a Mutex resource, but '{res}' has kind '{dk}'"),
                    Some(property_id),
                    None,
                ));
            }
        }
        "var_eq" | "var_cmp" => {
            let res = obj.get("resource").and_then(|v| v.as_str()).unwrap_or("");
            let dk = lookup_resource_kind(res, res_kind_map);
            if matches!(dk, "mutex" | "rwlock" | "condvar" | "semaphore" | "channel") {
                issues.push(SanityIssue::error(
                    "incompatible_resource_predicate",
                    format!("predicate '{pkind}' requires a modeled Var/Atomic resource, but '{res}' has kind '{dk}'"),
                    Some(property_id),
                    None,
                ));
            }
        }
        "channel_empty" | "channel_at_least" => {
            let res = obj.get("resource").and_then(|v| v.as_str()).unwrap_or("");
            let dk = lookup_resource_kind(res, res_kind_map);
            if !dk.is_empty() && dk != "channel" {
                issues.push(SanityIssue::error(
                    "incompatible_resource_predicate",
                    format!("predicate '{pkind}' requires a Channel resource, but '{res}' has kind '{dk}'"),
                    Some(property_id),
                    None,
                ));
            }
        }
        "not" => check_predicate_resource_compat(obj.get("predicate"), res_kind_map, property_id, issues),
        "and" | "or" => {
            if let Some(arr) = obj.get("predicates").and_then(|v| v.as_array()) {
                for sub in arr {
                    check_predicate_resource_compat(Some(sub), res_kind_map, property_id, issues);
                }
            }
        }
        _ => {}
    }
}

fn check_temporal_resource_compat(
    node: Option<&Value>,
    res_kind_map: &HashMap<String, String>,
    property_id: &str,
    issues: &mut Vec<SanityIssue>,
) {
    if res_kind_map.is_empty() {
        return;
    }
    let Some(obj) = node.and_then(|v| v.as_object()) else {
        return;
    };
    let op = obj
        .get("op")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if op == "atom" {
        check_predicate_resource_compat(obj.get("atom"), res_kind_map, property_id, issues);
    } else if obj.contains_key("arg") {
        check_temporal_resource_compat(obj.get("arg"), res_kind_map, property_id, issues);
    }
    if obj.contains_key("lhs") {
        check_temporal_resource_compat(obj.get("lhs"), res_kind_map, property_id, issues);
    }
    if obj.contains_key("rhs") {
        check_temporal_resource_compat(obj.get("rhs"), res_kind_map, property_id, issues);
    }
    if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
        for sub in args {
            check_temporal_resource_compat(Some(sub), res_kind_map, property_id, issues);
        }
    }
}

fn matches_grounded(name: &str, grounded: &BTreeSet<String>, allow_main: bool) -> bool {
    if grounded.is_empty() {
        return true;
    }
    let short = name.rsplit("::").next().unwrap_or(name);
    if allow_main && short == "main" {
        return true;
    }
    if grounded.contains(name) || grounded.contains(short) {
        return true;
    }
    grounded
        .iter()
        .any(|g| g.rsplit("::").next().unwrap_or(g) == short)
}

struct ParsedRequirements {
    clauses: HashMap<String, bool>,
    roles: BTreeSet<String>,
    resources: BTreeSet<String>,
    milestones: BTreeSet<String>,
    res_kind_map: HashMap<String, String>,
}

fn parse_requirements(req_doc: &Value) -> ParsedRequirements {
    let mut clauses = HashMap::new();
    if let Some(arr) = req_doc.get("clauses").and_then(|v| v.as_array()) {
        for c in arr {
            if let Some(id) = c.get("id").and_then(|v| v.as_str()).map(str::trim) {
                if !id.is_empty() {
                    let verifiable = c
                        .get("verifiable")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    clauses.insert(id.to_string(), verifiable);
                }
            }
        }
    }
    let mut roles = BTreeSet::new();
    let mut resources = BTreeSet::new();
    let mut milestones = BTreeSet::new();
    let mut res_kind_map = HashMap::new();
    if let Some(ent) = req_doc.get("entities").and_then(|v| v.as_object()) {
        let role_arr = ent
            .get("roles")
            .or_else(|| ent.get("functions"))
            .and_then(|v| v.as_array());
        if let Some(arr) = role_arr {
            for r in arr {
                if let Some(s) = r.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    roles.insert(s.to_string());
                }
            }
        }
        if let Some(arr) = ent.get("resources").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(s) = r.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    resources.insert(s.to_string());
                } else if let Some(obj) = r.as_object() {
                    if let Some(name) = obj.get("name").and_then(|v| v.as_str()).map(str::trim) {
                        if !name.is_empty() {
                            resources.insert(name.to_string());
                            let kind = obj
                                .get("kind")
                                .or_else(|| obj.get("type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .trim()
                                .to_ascii_lowercase();
                            if !kind.is_empty() {
                                let short = name.rsplit("::").next().unwrap_or(name);
                                res_kind_map.insert(short.to_string(), kind);
                            }
                        }
                    }
                }
            }
        }
        if let Some(arr) = ent.get("resource_specs").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(obj) = r.as_object() {
                    if let Some(name) = obj.get("name").and_then(|v| v.as_str()).map(str::trim) {
                        let kind = obj
                            .get("kind")
                            .or_else(|| obj.get("type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_ascii_lowercase();
                        if !name.is_empty() && !kind.is_empty() {
                            let short = name.rsplit("::").next().unwrap_or(name);
                            res_kind_map.insert(short.to_string(), kind);
                        }
                    }
                }
            }
        }
        if let Some(arr) = ent.get("milestones").and_then(|v| v.as_array()) {
            for m in arr {
                if let Some(s) = m.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    milestones.insert(s.to_string());
                }
            }
        }
    }
    ParsedRequirements {
        clauses,
        roles,
        resources,
        milestones,
        res_kind_map,
    }
}

fn validate_req_clauses(
    reqs: Option<&Value>,
    owner_kind: &str,
    owner_label: &str,
    property_id: &str,
    clauses: &HashMap<String, bool>,
    issues: &mut Vec<SanityIssue>,
) {
    if clauses.is_empty() {
        return;
    }
    let Some(arr) = reqs.and_then(|v| v.as_array()) else {
        return;
    };
    for cid in arr {
        let Some(cid_s) = cid.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        match clauses.get(cid_s) {
            None => issues.push(SanityIssue::error(
                "unknown_req_clause",
                format!("{owner_kind} '{owner_label}' references unknown clause '{cid_s}'"),
                Some(property_id),
                Some(cid_s),
            )),
            Some(false) => {
                let hole_suffix = if owner_kind == "property" {
                    "/sequential_hole"
                } else {
                    ""
                };
                issues.push(SanityIssue::error(
                    "unverifiable_req_clause",
                    format!(
                        "{owner_kind} '{owner_label}' references unverifiable{hole_suffix} clause '{cid_s}'"
                    ),
                    Some(property_id),
                    Some(cid_s),
                ));
            }
            Some(true) => {}
        }
    }
}

fn validate_entity_grounding(
    funcs: &BTreeSet<String>,
    resources: &BTreeSet<String>,
    milestones: Option<&BTreeSet<String>>,
    owner_kind: &str,
    owner_label: &str,
    req: &ParsedRequirements,
    strict_entities: bool,
    issues: &mut Vec<SanityIssue>,
) {
    let sev = if strict_entities { "error" } else { "warning" };
    let roles_dbg: Vec<&str> = req.roles.iter().map(String::as_str).collect();
    let res_dbg: Vec<&str> = req.resources.iter().map(String::as_str).collect();
    for fn_name in funcs {
        if !matches_grounded(fn_name, &req.roles, true) {
            issues.push(SanityIssue::with_severity(
                sev,
                "ungrounded_function",
                format!(
                    "{owner_kind} '{owner_label}' references function '{fn_name}' not in grounded roles {roles_dbg:?}"
                ),
                Some(owner_label),
                None,
            ));
        }
    }
    for res_name in resources {
        if !matches_grounded(res_name, &req.resources, false) {
            issues.push(SanityIssue::with_severity(
                sev,
                "ungrounded_resource",
                format!(
                    "{owner_kind} '{owner_label}' references resource '{res_name}' not in grounded resources {res_dbg:?}"
                ),
                Some(owner_label),
                None,
            ));
        }
    }
    if let Some(ms_set) = milestones {
        if !req.milestones.is_empty() {
            let ms_dbg: Vec<&str> = req.milestones.iter().map(String::as_str).collect();
            for ms in ms_set {
                if !req.milestones.contains(ms) {
                    issues.push(SanityIssue::with_severity(
                        sev,
                        "ungrounded_milestone",
                        format!(
                            "{owner_kind} '{owner_label}' references milestone '{ms}' not in grounded milestones {ms_dbg:?}"
                        ),
                        Some(owner_label),
                        None,
                    ));
                }
            }
        }
    }
}

fn validate_fairness_list(
    fairness: Option<&Value>,
    functions: &mut BTreeSet<String>,
    resources: &mut BTreeSet<String>,
    milestones: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let Some(arr) = fairness.and_then(|v| v.as_array()) else {
        return;
    };
    for fc in arr {
        let Some(obj) = fc.as_object() else {
            errors.push("fairness entry must be an object".into());
            continue;
        };
        let fkind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match fkind.as_str() {
            "justice" => collect_predicate_refs(
                obj.get("atom"),
                functions,
                resources,
                Some(milestones),
                errors,
            ),
            "compassion" => {
                collect_predicate_refs(
                    obj.get("enabled"),
                    functions,
                    resources,
                    Some(milestones),
                    errors,
                );
                collect_predicate_refs(
                    obj.get("taken"),
                    functions,
                    resources,
                    Some(milestones),
                    errors,
                );
            }
            _ => errors.push(format!("unknown fairness kind '{fkind}'")),
        }
    }
}

/// Authoritative contract sanity validator shared by CLI & Python Stage 0.
pub fn validate_contract_value(
    contract: &Value,
    req_doc: Option<&Value>,
    strict_entities: bool,
    require_non_vacuous: bool,
    normalize: bool,
) -> SanityReport {
    let effective = if normalize {
        ensure_implication_non_vacuous_value(contract)
    } else {
        contract.clone()
    };
    let mut issues = Vec::new();
    let Some(obj) = effective.as_object() else {
        issues.push(SanityIssue::error(
            "invalid_contract",
            "contract must be a JSON object",
            None,
            None,
        ));
        return SanityReport {
            schema: CONTRACT_VALIDATION_SCHEMA.into(),
            ok: false,
            checked_properties: 0,
            checked_preserved: 0,
            issues,
            normalized_contract: normalize.then_some(effective),
        };
    };

    let empty_props = Vec::new();
    let props: &Vec<Value> = match obj.get("properties") {
        None => {
            if obj.get("deadlock_free").and_then(|v| v.as_bool()) == Some(true) {
                &empty_props
            } else {
                issues.push(SanityIssue::error(
                    "missing_properties",
                    "contract must contain a 'properties' list",
                    None,
                    None,
                ));
                return SanityReport {
                    schema: CONTRACT_VALIDATION_SCHEMA.into(),
                    ok: false,
                    checked_properties: 0,
                    checked_preserved: 0,
                    issues,
                    normalized_contract: normalize.then_some(effective),
                };
            }
        }
        Some(v) => match v.as_array() {
            Some(arr) => arr,
            None => {
                issues.push(SanityIssue::error(
                    "invalid_properties",
                    "'properties' must be a list",
                    None,
                    None,
                ));
                return SanityReport {
                    schema: CONTRACT_VALIDATION_SCHEMA.into(),
                    ok: false,
                    checked_properties: 0,
                    checked_preserved: 0,
                    issues,
                    normalized_contract: normalize.then_some(effective),
                };
            }
        },
    };

    let parsed_req = req_doc.map(parse_requirements);
    let mut seen_ids = HashSet::new();

    for (idx, prop) in props.iter().enumerate() {
        let Some(pobj) = prop.as_object() else {
            issues.push(SanityIssue::error(
                "invalid_property",
                format!("property[{idx}] must be a JSON object"),
                None,
                None,
            ));
            continue;
        };
        let pid = pobj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if pid.is_empty() {
            issues.push(SanityIssue::error(
                "empty_property_id",
                format!("property[{idx}] has empty 'id'"),
                None,
                None,
            ));
        } else if !seen_ids.insert(pid.to_string()) {
            issues.push(SanityIssue::error(
                "duplicate_property_id",
                format!("duplicate property id '{pid}'"),
                Some(pid),
                None,
            ));
        }

        let kind = pobj
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !is_valid_property_kind(&kind) {
            issues.push(SanityIssue::error(
                "unknown_property_kind",
                format!("property '{pid}' has unknown kind '{kind}'"),
                Some(pid),
                None,
            ));
            continue;
        }

        if let Some(req) = &parsed_req {
            validate_req_clauses(
                pobj.get("req"),
                "property",
                pid,
                pid,
                &req.clauses,
                &mut issues,
            );
        }

        let res_kind_map = parsed_req
            .as_ref()
            .map(|r| &r.res_kind_map)
            .cloned()
            .unwrap_or_default();

        let mut tree_errors = Vec::new();
        let mut p_funcs = BTreeSet::new();
        let mut p_res = BTreeSet::new();
        let mut p_ms = BTreeSet::new();

        match kind.as_str() {
            "safety" => {
                collect_predicate_refs(
                    pobj.get("invariant"),
                    &mut p_funcs,
                    &mut p_res,
                    Some(&mut p_ms),
                    &mut tree_errors,
                );
                check_predicate_resource_compat(pobj.get("invariant"), &res_kind_map, pid, &mut issues);
            }
            "reachability" | "always_reachable" => {
                collect_predicate_refs(
                    pobj.get("goal"),
                    &mut p_funcs,
                    &mut p_res,
                    Some(&mut p_ms),
                    &mut tree_errors,
                );
                check_predicate_resource_compat(pobj.get("goal"), &res_kind_map, pid, &mut issues);
            }
            "unreachable" => {
                collect_predicate_refs(
                    pobj.get("bad"),
                    &mut p_funcs,
                    &mut p_res,
                    Some(&mut p_ms),
                    &mut tree_errors,
                );
                check_predicate_resource_compat(pobj.get("bad"), &res_kind_map, pid, &mut issues);
            }
            "ctl" => {
                validate_temporal_tree(
                    pobj.get("formula"),
                    "ctl",
                    &mut p_funcs,
                    &mut p_res,
                    Some(&mut p_ms),
                    &mut tree_errors,
                );
                check_temporal_resource_compat(pobj.get("formula"), &res_kind_map, pid, &mut issues);
                if require_non_vacuous
                    && pobj.get("formula").is_some_and(formula_contains_implication_value)
                    && pobj.get("non_vacuous").and_then(|v| v.as_bool()) != Some(true)
                {
                    issues.push(SanityIssue::error(
                        "missing_non_vacuous",
                        format!("CTL property '{pid}' contains an implication and must set 'non_vacuous': true"),
                        Some(pid),
                        None,
                    ));
                }
            }
            "ltl" => {
                validate_temporal_tree(
                    pobj.get("formula"),
                    "ltl",
                    &mut p_funcs,
                    &mut p_res,
                    Some(&mut p_ms),
                    &mut tree_errors,
                );
                check_temporal_resource_compat(pobj.get("formula"), &res_kind_map, pid, &mut issues);
                validate_fairness_list(
                    pobj.get("fairness"),
                    &mut p_funcs,
                    &mut p_res,
                    &mut p_ms,
                    &mut tree_errors,
                );
                if require_non_vacuous
                    && pobj.get("formula").is_some_and(formula_contains_implication_value)
                    && pobj.get("non_vacuous").and_then(|v| v.as_bool()) != Some(true)
                {
                    issues.push(SanityIssue::error(
                        "missing_non_vacuous",
                        format!("LTL property '{pid}' contains an implication and must set 'non_vacuous': true"),
                        Some(pid),
                        None,
                    ));
                }
            }
            "pattern" => {
                let pat = pobj
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if !is_valid_pattern_kind(&pat) {
                    tree_errors.push(format!("unknown pattern kind '{pat}'"));
                } else {
                    match pat.as_str() {
                        "always" => {
                            let cond = pobj.get("cond").or_else(|| pobj.get("invariant"));
                            collect_predicate_refs(cond, &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            check_predicate_resource_compat(cond, &res_kind_map, pid, &mut issues);
                        }
                        "exclude" => match pobj.get("preds").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
                            None => tree_errors.push("pattern 'exclude' requires non-empty list 'preds'".into()),
                            Some(arr) => {
                                for sub in arr {
                                    collect_predicate_refs(Some(sub), &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                                    check_predicate_resource_compat(Some(sub), &res_kind_map, pid, &mut issues);
                                }
                            }
                        },
                        "at_most" => {
                            match pobj.get("conds").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
                                None => tree_errors.push("pattern 'at_most' requires non-empty list 'conds'".into()),
                                Some(arr) => {
                                    for sub in arr {
                                        collect_predicate_refs(Some(sub), &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                                        check_predicate_resource_compat(Some(sub), &res_kind_map, pid, &mut issues);
                                    }
                                }
                            }
                            if pobj.get("max").and_then(|v| v.as_u64()).is_none() {
                                tree_errors.push("pattern 'at_most' requires non-negative int 'max'".into());
                            }
                        }
                        "precedence" => {
                            collect_predicate_refs(pobj.get("effect"), &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            collect_predicate_refs(pobj.get("cause"), &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            check_predicate_resource_compat(pobj.get("effect"), &res_kind_map, pid, &mut issues);
                            check_predicate_resource_compat(pobj.get("cause"), &res_kind_map, pid, &mut issues);
                            if require_non_vacuous && pobj.get("non_vacuous").and_then(|v| v.as_bool()) == Some(false) {
                                issues.push(SanityIssue::error(
                                    "missing_non_vacuous",
                                    format!("Pattern property '{pid}' ('precedence') should keep 'non_vacuous': true"),
                                    Some(pid),
                                    None,
                                ));
                            }
                        }
                        "triggers" => {
                            let cau = pobj.get("cause").or_else(|| pobj.get("trigger"));
                            let eff = pobj.get("effect").or_else(|| pobj.get("response"));
                            collect_predicate_refs(cau, &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            collect_predicate_refs(eff, &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            check_predicate_resource_compat(cau, &res_kind_map, pid, &mut issues);
                            check_predicate_resource_compat(eff, &res_kind_map, pid, &mut issues);
                            validate_fairness_list(pobj.get("fairness"), &mut p_funcs, &mut p_res, &mut p_ms, &mut tree_errors);
                            if require_non_vacuous
                                && pobj.contains_key("non_vacuous")
                                && pobj.get("non_vacuous").and_then(|v| v.as_bool()) != Some(true)
                            {
                                issues.push(SanityIssue::error(
                                    "missing_non_vacuous",
                                    format!("Pattern property '{pid}' ('triggers') contains an implication and must set 'non_vacuous': true"),
                                    Some(pid),
                                    None,
                                ));
                            }
                        }
                        "can_reach" | "always_can_reach" => {
                            collect_predicate_refs(pobj.get("goal"), &mut p_funcs, &mut p_res, Some(&mut p_ms), &mut tree_errors);
                            check_predicate_resource_compat(pobj.get("goal"), &res_kind_map, pid, &mut issues);
                        }
                        _ => {}
                    }
                }
            }
            "health" => {
                if let Some(checks) = pobj.get("checks") {
                    match checks.as_array() {
                        None => tree_errors.push("health 'checks' must be a list".into()),
                        Some(arr) => {
                            for c in arr {
                                let ck = c.as_str().unwrap_or("").trim().to_ascii_lowercase();
                                if !is_valid_health_check(&ck) {
                                    tree_errors.push(format!("unknown health bug kind '{ck}'"));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        for err in tree_errors {
            issues.push(SanityIssue::error(
                "malformed_formula",
                format!("property '{pid}': {err}"),
                Some(pid),
                None,
            ));
        }

        if let Some(req) = &parsed_req {
            validate_entity_grounding(
                &p_funcs,
                &p_res,
                Some(&p_ms),
                "property",
                pid,
                req,
                strict_entities,
                &mut issues,
            );
        }
    }

    let mut checked_preserved = 0usize;
    let empty_pres = Vec::new();
    let pres_list: &Vec<Value> = match obj.get("preserved") {
        None => &empty_pres,
        Some(v) => match v.as_array() {
            Some(arr) => arr,
            None => {
                issues.push(SanityIssue::error(
                    "invalid_preserved",
                    "'preserved' must be a list",
                    None,
                    None,
                ));
                &empty_pres
            }
        },
    };
    checked_preserved = pres_list.len();
    for (idx, pr) in pres_list.iter().enumerate() {
        let Some(pr_obj) = pr.as_object() else {
            issues.push(SanityIssue::error(
                "invalid_preserved_entry",
                format!("preserved[{idx}] must be a JSON object"),
                None,
                None,
            ));
            continue;
        };
        let pkind = pr_obj
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let fallback = format!("preserved[{idx}]");
        let p_label = pr_obj
            .get("description")
            .or_else(|| pr_obj.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or(&fallback);
        if let Some(req) = &parsed_req {
            validate_req_clauses(
                pr_obj.get("req"),
                "preserved",
                p_label,
                p_label,
                &req.clauses,
                &mut issues,
            );
        }
        let res_kind_map = parsed_req
            .as_ref()
            .map(|r| &r.res_kind_map)
            .cloned()
            .unwrap_or_default();
        let mut pr_errors = Vec::new();
        let mut pr_funcs = BTreeSet::new();
        let mut pr_res = BTreeSet::new();
        match pkind.as_str() {
            "reachable" => {
                collect_predicate_refs(pr_obj.get("goal"), &mut pr_funcs, &mut pr_res, None, &mut pr_errors);
                check_predicate_resource_compat(pr_obj.get("goal"), &res_kind_map, p_label, &mut issues);
            }
            "always" => {
                collect_predicate_refs(pr_obj.get("invariant"), &mut pr_funcs, &mut pr_res, None, &mut pr_errors);
                check_predicate_resource_compat(pr_obj.get("invariant"), &res_kind_map, p_label, &mut issues);
            }
            "function_completed" => {
                if let Some(f) = pr_obj.get("function").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    pr_funcs.insert(f.to_string());
                }
            }
            _ => pr_errors.push(format!("unknown preserved kind '{pkind}'")),
        }
        for err in pr_errors {
            issues.push(SanityIssue::error(
                "malformed_preserved",
                format!("preserved '{p_label}': {err}"),
                Some(p_label),
                None,
            ));
        }
        if let Some(req) = &parsed_req {
            validate_entity_grounding(
                &pr_funcs,
                &pr_res,
                None,
                "preserved",
                p_label,
                req,
                strict_entities,
                &mut issues,
            );
        }
    }

    let ok = !issues.iter().any(|i| i.severity == "error");
    SanityReport {
        schema: CONTRACT_VALIDATION_SCHEMA.into(),
        ok,
        checked_properties: props.len(),
        checked_preserved,
        issues,
        normalized_contract: normalize.then_some(effective),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ensure_implication_non_vacuous_sets_default_true() {
        let raw = json!({
            "name": "c",
            "properties": [
                {
                    "kind": "ctl",
                    "id": "p1",
                    "formula": {
                        "op": "ag",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "var_cmp", "resource": "main::x", "op": ">", "value": 0}},
                            "rhs": {"op": "af", "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::x", "value": 0}}}
                        }
                    }
                },
                {
                    "kind": "pattern",
                    "id": "p2",
                    "pattern": "triggers",
                    "cause": {"kind": "mutex_held", "resource": "main::m"},
                    "effect": {"kind": "mutex_free", "resource": "main::m"}
                }
            ]
        });
        let norm = ensure_implication_non_vacuous_value(&raw);
        assert_eq!(norm["properties"][0]["non_vacuous"], json!(true));
        assert_eq!(norm["properties"][1]["non_vacuous"], json!(true));
    }

    #[test]
    fn validate_contract_catches_all_sanity_error_classes() {
        let req = json!({
            "entities": {
                "roles": ["reader", "writer"],
                "resources": [
                    {"name": "book_lock", "kind": "RwLock"},
                    {"name": "pending", "kind": "Var"}
                ]
            },
            "clauses": [
                {"id": "R1", "verifiable": true},
                {"id": "R4", "verifiable": false}
            ]
        });
        let bad = json!({
            "name": "broken",
            "properties": [
                {"kind": "deadlock_free", "id": "dup", "req": ["R1"]},
                {"kind": "deadlock_free", "id": "dup", "req": ["R99"]},
                {
                    "kind": "safety",
                    "id": "bad-kind",
                    "invariant": {"kind": "mutex_held", "resource": "main::pending"},
                    "req": ["R4"]
                },
                {
                    "kind": "ctl",
                    "id": "vac",
                    "non_vacuous": false,
                    "formula": {
                        "op": "ag",
                        "arg": {
                            "op": "implies",
                            "lhs": {"op": "atom", "atom": {"kind": "function_completed", "function": "main::ghost_fn"}},
                            "rhs": {"op": "af", "arg": {"op": "atom", "atom": {"kind": "var_eq", "resource": "main::ghost_var", "value": 0}}}
                        }
                    }
                }
            ]
        });
        let rep = validate_contract_value(&bad, Some(&req), true, true, false);
        assert!(!rep.ok);
        let codes: HashSet<&str> = rep.issues.iter().map(|i| i.code.as_str()).collect();
        for expected in [
            "duplicate_property_id",
            "unknown_req_clause",
            "unverifiable_req_clause",
            "incompatible_resource_predicate",
            "ungrounded_function",
            "ungrounded_resource",
            "missing_non_vacuous",
        ] {
            assert!(codes.contains(expected), "missing {expected} in {codes:?}");
        }
    }
}
