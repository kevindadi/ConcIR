pub mod compat;
pub mod concurrency;
pub mod control;
pub mod locks;
pub mod names;
pub mod protection;
pub mod summary;
pub mod types;

use crate::ast::Program;
use crate::diagnostic::{Severity, ValidationReport};

/// Run all validation passes on a parsed CIR program, returning the full report.
pub fn validate(program: &Program, source: &str) -> ValidationReport {
    let mut diags = Vec::new();

    names::check(program, source, &mut diags);
    types::check(program, source, &mut diags);
    compat::check(program, source, &mut diags);
    protection::check(program, source, &mut diags);
    concurrency::check(program, source, &mut diags);
    locks::check(program, source, &mut diags);
    control::check(program, source, &mut diags);
    summary::check(program, source, &mut diags);

    let valid = !diags.iter().any(|d| d.severity == Severity::Error);

    ValidationReport {
        valid,
        diagnostics: diags,
    }
}
