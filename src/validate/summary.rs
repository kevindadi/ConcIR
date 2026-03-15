use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostic::Diagnostic;

/// E8xx: FnSummary consistency checks.
pub fn check(program: &Program, source: &str, diags: &mut Vec<Diagnostic>) {
    let resource_names: HashSet<&str> = program
        .resources
        .iter()
        .map(|r| r.name.value.as_str())
        .collect();

    let function_names: HashSet<&str> = program
        .functions
        .iter()
        .map(|f| f.name.value.as_str())
        .chain(program.fn_summaries.iter().map(|s| s.name.value.as_str()))
        .collect();

    let fn_body_names: HashSet<&str> = program
        .functions
        .iter()
        .map(|f| f.name.value.as_str())
        .collect();

    // Build a map of has_concurrency for summaries
    let summary_concurrency: HashMap<&str, bool> = program
        .fn_summaries
        .iter()
        .map(|s| (s.name.value.as_str(), s.has_concurrency))
        .collect();

    for s in &program.fn_summaries {
        // E803: fn has both body and summary
        if fn_body_names.contains(s.name.value.as_str()) {
            diags.push(
                Diagnostic::error(
                    "E803",
                    format!(
                        "function '{}' has both a fn body and an fn_summary",
                        s.name.value
                    ),
                )
                .with_span(s.name.span, source)
                .with_fix("remove the fn_summary; let the tool compute it from the body"),
            );
        }

        // E801: reads/writes reference non-existent resources
        for r in &s.reads {
            if !resource_names.contains(r.value.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "fn_summary '{}' reads resource '{}' which is not declared",
                            s.name.value, r.value
                        ),
                    )
                    .with_span(r.span, source)
                    .with_fix("correct the resource name or add it to the resources block"),
                );
            }
        }
        for w in &s.writes {
            if !resource_names.contains(w.value.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E801",
                        format!(
                            "fn_summary '{}' writes resource '{}' which is not declared",
                            s.name.value, w.value
                        ),
                    )
                    .with_span(w.span, source)
                    .with_fix("correct the resource name or add it to the resources block"),
                );
            }
        }

        // E802: callees reference non-existent functions
        for c in &s.callees {
            if !function_names.contains(c.value.as_str()) {
                diags.push(
                    Diagnostic::error(
                        "E802",
                        format!(
                            "fn_summary '{}' lists callee '{}' which has no fn or fn_summary",
                            s.name.value, c.value
                        ),
                    )
                    .with_span(c.span, source)
                    .with_fix("add a fn definition or fn_summary for this callee"),
                );
            }
        }

        // E804: has_concurrency should propagate from callees
        if !s.has_concurrency {
            let callee_has_concurrency = s.callees.iter().any(|c| {
                summary_concurrency
                    .get(c.value.as_str())
                    .copied()
                    .unwrap_or(false)
            });
            // Also check if any callee fn body has spawn/spawn_async
            let callee_body_concurrent = s.callees.iter().any(|c| {
                program
                    .functions
                    .iter()
                    .find(|f| f.name.value == c.value)
                    .map(|f| {
                        f.statements.iter().any(|st| {
                            matches!(st.op, Op::Spawn(_) | Op::SpawnAsync(_))
                        })
                    })
                    .unwrap_or(false)
            });

            if callee_has_concurrency || callee_body_concurrent {
                diags.push(
                    Diagnostic::error(
                        "E804",
                        format!(
                            "fn_summary '{}' has has_concurrency=false but a callee has concurrency",
                            s.name.value
                        ),
                    )
                    .with_span(s.name.span, source)
                    .with_fix("set has_concurrency to true"),
                );
            }
        }
    }
}
