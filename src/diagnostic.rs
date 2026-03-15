use serde::Serialize;

use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanInfo {
    pub offset: usize,
    pub length: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub diagnostics: Vec<Diagnostic>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            span: None,
            fix_hint: None,
        }
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            span: None,
            fix_hint: None,
        }
    }

    pub fn with_span(mut self, span: Span, source: &str) -> Self {
        let (line, column) = span.line_col(source);
        self.span = Some(SpanInfo {
            offset: span.offset,
            length: span.length,
            line,
            column,
        });
        self
    }

    pub fn with_fix(mut self, hint: impl Into<String>) -> Self {
        self.fix_hint = Some(hint.into());
        self
    }
}
