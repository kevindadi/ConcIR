use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E8xx: FnSummary consistency checks.
pub fn check(program: &Program, diags: &mut Vec<Diagnostic>) {
    let resource_names: HashSet<&str> = program.resources.iter().map(|r| r.name.as_str()).collect();

    let function_names: HashSet<&str> = program
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .chain(program.fn_summaries.iter().map(|s| s.name.as_str()))
        .collect();

    let fn_body_names: HashSet<&str> = program.functions.iter().map(|f| f.name.as_str()).collect();

    let summary_concurrency: HashMap<&str, bool> = program
        .fn_summaries
        .iter()
        .map(|s| (s.name.as_str(), s.has_concurrency))
        .collect();

    for (si, s) in program.fn_summaries.iter().enumerate() {
        let sum_path = format!("fn_summaries[{si}]");

        // E803: fn has both body and summary
        if fn_body_names.contains(s.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    "E803",
                    format!("function '{}' has both a fn body and an fn_summary", s.name),
                )
                .with_path(sum_path.clone())
                .with_fix("remove the fn_summary; let the tool compute it from the body"),
            );
        }

        // E801: reads/writes reference non-existent resources
        for (ri, r) in s.reads.iter().enumerate() {
            if !resource_names.contains(r.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "fn_summary '{}' reads resource '{r}' which is not declared",
                            s.name
                        ),
                    )
                    .with_path(format!("{sum_path}.reads[{ri}]"))
                    .with_fix("correct the resource name or add it to the resources block"),
                );
            }
        }
        for (wi, w) in s.writes.iter().enumerate() {
            if !resource_names.contains(w.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "fn_summary '{}' writes resource '{w}' which is not declared",
                            s.name
                        ),
                    )
                    .with_path(format!("{sum_path}.writes[{wi}]"))
                    .with_fix("correct the resource name or add it to the resources block"),
                );
            }
        }

        // E802: callees reference non-existent functions
        for (ci, c) in s.callees.iter().enumerate() {
            if !function_names.contains(c.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E802",
                        format!(
                            "fn_summary '{}' lists callee '{c}' which has no fn or fn_summary",
                            s.name
                        ),
                    )
                    .with_path(format!("{sum_path}.callees[{ci}]"))
                    .with_fix("add a fn definition or fn_summary for this callee"),
                );
            }
        }

        // E804: has_concurrency should propagate from callees
        if !s.has_concurrency {
            let callee_has_concurrency = s.callees.iter().any(|c| {
                summary_concurrency
                    .get(c.as_str())
                    .copied()
                    .unwrap_or(false)
            });

            let callee_body_concurrent = s.callees.iter().any(|c| {
                program
                    .functions
                    .iter()
                    .find(|f| f.name == *c)
                    .map(|f| {
                        f.body
                            .iter()
                            .any(|st| matches!(st.op, Op::Spawn(_) | Op::SpawnAsync(_)))
                    })
                    .unwrap_or(false)
            });

            if callee_has_concurrency || callee_body_concurrent {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "fn_summary '{}' has has_concurrency=false but a callee has concurrency",
                            s.name
                        ),
                    )
                    .with_path(format!("{sum_path}.has_concurrency"))
                    .with_fix("set has_concurrency to true"),
                );
            }
        }
    }
}
