pub mod compat;
pub mod concurrency;
pub mod control;
pub mod locks;
pub mod names;
pub mod protection;
pub mod structure;
pub mod summary;
pub mod types;

use crate::ast::Program;
use crate::diagnostic::{Severity, ValidationReport};

/// Run all validation passes on a parsed CIR program, returning the full report.
pub fn validate(program: &Program) -> ValidationReport {
    let mut diags = Vec::new();

    structure::check(program, &mut diags);
    names::check(program, &mut diags);
    types::check(program, &mut diags);
    compat::check(program, &mut diags);
    protection::check(program, &mut diags);
    concurrency::check(program, &mut diags);
    locks::check(program, &mut diags);
    control::check(program, &mut diags);
    summary::check(program, &mut diags);

    let valid = !diags.iter().any(|d| d.severity == Severity::Error);

    ValidationReport {
        valid,
        diagnostics: diags,
    }
}
