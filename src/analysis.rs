use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rustc_hir::{def::DefKind, intravisit::Visitor, Item, ItemKind};
use rustc_middle::ty::TyCtxt;
use rustc_span::{Pos, Span};
use serde::Serialize;

pub struct AsyncAnalysisContext<'tcx> {
    pub tcx: TyCtxt<'tcx>,
}

#[derive(Debug, Default, Clone)]
pub struct AnalyzerOptions {
    pub json_report_path: Option<PathBuf>,
}

pub trait AfterAnalysisPass {
    fn run<'tcx>(&mut self, cx: &AsyncAnalysisContext<'tcx>) -> Result<()>;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceLocation {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BugDiagnostic {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub location: Option<SourceLocation>,
}

pub struct RuleContext<'tcx> {
    tcx: TyCtxt<'tcx>,
}

impl<'tcx> RuleContext<'tcx> {
    pub fn tcx(&self) -> TyCtxt<'tcx> {
        self.tcx
    }

    pub fn crate_name(&self) -> String {
        self.tcx.crate_name(rustc_hir::def_id::LOCAL_CRATE).to_string()
    }

    pub fn location_from_span(&self, span: Span) -> Option<SourceLocation> {
        if span.is_dummy() {
            return None;
        }
        let source_map = self.tcx.sess.source_map();
        let loc = source_map.lookup_char_pos(span.lo());
        Some(SourceLocation {
            file: loc
                .file
                .name
                .prefer_local_unconditionally()
                .to_string_lossy()
                .into_owned(),
            line: loc.line,
            column: loc.col.to_usize() + 1,
        })
    }

    pub fn make_diagnostic(
        &self,
        rule_id: &str,
        severity: Severity,
        message: impl Into<String>,
        span: Option<Span>,
    ) -> BugDiagnostic {
        BugDiagnostic {
            rule_id: rule_id.to_string(),
            severity,
            message: message.into(),
            location: span.and_then(|s| self.location_from_span(s)),
        }
    }
}

pub trait AsyncBugRule: Send {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str {
        ""
    }
    fn check<'tcx>(&mut self, cx: &RuleContext<'tcx>, out: &mut Vec<BugDiagnostic>) -> Result<()>;
}

#[derive(Default)]
pub struct RuleRegistry {
    rules: Vec<Box<dyn AsyncBugRule>>,
}

impl RuleRegistry {
    pub fn register<R>(&mut self, rule: R)
    where
        R: AsyncBugRule + 'static,
    {
        self.rules.push(Box::new(rule));
    }

    pub fn run_all<'tcx>(&mut self, cx: &RuleContext<'tcx>) -> Result<Vec<BugDiagnostic>> {
        let mut out = Vec::new();
        for rule in &mut self.rules {
            rule.check(cx, &mut out)
                .with_context(|| format!("rule `{}` ({}) failed", rule.id(), rule.description()))?;
        }
        Ok(out)
    }
}

#[derive(Debug, Serialize)]
pub struct JsonReport {
    pub crate_name: String,
    pub diagnostics: Vec<BugDiagnostic>,
}

#[derive(Default)]
pub struct JsonReporter {
    path: Option<PathBuf>,
}

impl JsonReporter {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    pub fn write(&self, report: &JsonReport) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create report directory `{}`", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(report).context("failed to encode json report")?;
        fs::write(path, payload)
            .with_context(|| format!("failed to write json report `{}`", path.display()))?;
        Ok(())
    }
}

pub struct AsyncBugPass {
    registry: RuleRegistry,
    reporter: JsonReporter,
}

impl AsyncBugPass {
    pub fn new(registry: RuleRegistry, reporter: JsonReporter) -> Self {
        Self { registry, reporter }
    }

    fn print_diagnostics(diagnostics: &[BugDiagnostic]) {
        for diag in diagnostics {
            let location = diag
                .location
                .as_ref()
                .map(|l| format!("{}:{}:{}", l.file, l.line, l.column))
                .unwrap_or_else(|| "<unknown>".to_string());
            eprintln!(
                "[async-bugs][{:?}][{}] {} @ {}",
                diag.severity, diag.rule_id, diag.message, location
            );
        }
    }
}

impl AfterAnalysisPass for AsyncBugPass {
    fn run<'tcx>(&mut self, cx: &AsyncAnalysisContext<'tcx>) -> Result<()> {
        let rule_cx = RuleContext { tcx: cx.tcx };
        let diagnostics = self.registry.run_all(&rule_cx)?;
        Self::print_diagnostics(&diagnostics);
        let report = JsonReport {
            crate_name: rule_cx.crate_name(),
            diagnostics,
        };
        self.reporter.write(&report)?;
        Ok(())
    }
}

pub fn default_async_bug_pass(options: AnalyzerOptions) -> AsyncBugPass {
    let mut registry = RuleRegistry::default();
    registry.register(ListAsyncFnsRule);
    AsyncBugPass::new(registry, JsonReporter::new(options.json_report_path))
}

pub struct ListAsyncFnsRule;

impl AsyncBugRule for ListAsyncFnsRule {
    fn id(&self) -> &'static str {
        "demo.list_async_fns"
    }

    fn description(&self) -> &'static str {
        "Collect async functions as demo diagnostics."
    }

    fn check<'tcx>(&mut self, cx: &RuleContext<'tcx>, out: &mut Vec<BugDiagnostic>) -> Result<()> {
        let mut visitor = AsyncFnVisitor::new(cx);
        cx.tcx().hir_walk_toplevel_module(&mut visitor);
        out.extend(visitor.diagnostics);
        Ok(())
    }
}

struct AsyncFnVisitor<'a, 'tcx> {
    cx: &'a RuleContext<'tcx>,
    diagnostics: Vec<BugDiagnostic>,
}

impl<'a, 'tcx> AsyncFnVisitor<'a, 'tcx> {
    fn new(cx: &'a RuleContext<'tcx>) -> Self {
        Self {
            cx,
            diagnostics: Vec::new(),
        }
    }
}

impl<'a, 'tcx> Visitor<'tcx> for AsyncFnVisitor<'a, 'tcx> {
    fn visit_item(&mut self, item: &'tcx Item<'tcx>) {
        if let ItemKind::Fn { .. } = item.kind {
            let def_id = item.owner_id.def_id.to_def_id();
            if self.cx.tcx().def_kind(def_id) == DefKind::Fn && self.cx.tcx().asyncness(def_id).is_async()
            {
                let name = self.cx.tcx().def_path_str(def_id);
                self.diagnostics.push(self.cx.make_diagnostic(
                    "demo.list_async_fns",
                    Severity::Info,
                    format!("found async fn `{name}`"),
                    Some(item.span),
                ));
            }
        }
        rustc_hir::intravisit::walk_item(self, item);
    }
}
