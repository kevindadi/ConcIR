pub mod compat;
pub mod concurrency;
pub mod control;
pub mod dataflow;
pub mod interface;
pub mod locks;
pub mod names;
pub mod protection;
pub mod structure;
pub mod types;

use crate::ast::Program;
use crate::diagnostic::{Severity, ValidationReport};

/// Run all validation passes on a parsed ConcIR program, returning the full report.
/// Order: E0xx → E1xx → E2xx → E3xx → E7xx → E4xx → E5xx → E8xx → E6xx → E9xx.
pub fn validate(program: &Program) -> ValidationReport {
    let mut diags = Vec::new();

    structure::check(program, &mut diags);
    names::check(program, &mut diags);
    crate::typedef::check(program, &mut diags);
    types::check(program, &mut diags);
    compat::check(program, &mut diags);
    protection::check(program, &mut diags);
    concurrency::check(program, &mut diags);
    locks::check(program, &mut diags);
    interface::check(program, &mut diags);
    control::check(program, &mut diags);
    dataflow::check(program, &mut diags);

    let valid = !diags.iter().any(|d| d.severity == Severity::Error);

    ValidationReport {
        valid,
        diagnostics: diags,
    }
}
