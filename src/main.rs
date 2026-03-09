#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod analysis;

use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use analysis::{default_async_bug_pass, AfterAnalysisPass, AnalyzerOptions, AsyncAnalysisContext};
use anyhow::{anyhow, Context, Result};
use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface;

struct AnalyzerCallbacks<P> {
    pass: P,
}

impl<P> AnalyzerCallbacks<P> {
    fn new(pass: P) -> Self {
        Self { pass }
    }
}

impl<P> Callbacks for AnalyzerCallbacks<P>
where
    P: AfterAnalysisPass + Send,
{
    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: rustc_middle::ty::TyCtxt<'tcx>,
    ) -> Compilation {
        let cx = AsyncAnalysisContext { tcx };
        if let Err(err) = self.pass.run(&cx) {
            eprintln!("[after_analysis] analyzer error: {err:#}");
        }
        Compilation::Continue
    }
}

fn main() {
    rustc_driver::install_ice_hook(rustc_driver::DEFAULT_BUG_REPORT_URL, |_| ());

    if let Err(err) = run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let input = normalized_driver_input()?;
    if input.rustc_args.len() <= 1 {
        return Err(anyhow!(
            "missing rustc arguments; pass rustc args directly or run via RUSTC_WORKSPACE_WRAPPER"
        ));
    }

    let mut callbacks = AnalyzerCallbacks::new(default_async_bug_pass(input.analyzer_options));
    rustc_driver::run_compiler(&input.rustc_args, &mut callbacks);
    Ok(())
}

struct DriverInput {
    rustc_args: Vec<String>,
    analyzer_options: AnalyzerOptions,
}

fn normalized_driver_input() -> Result<DriverInput> {
    let mut raw_args: Vec<String> = env::args().skip(1).collect();

    // Cargo workspace wrapper mode: first arg is the real rustc path.
    if let Some(first) = raw_args.first() {
        if looks_like_rustc_path(first) {
            raw_args.remove(0);
        }
    }

    let mut analyzer_options = AnalyzerOptions::default();
    let mut rustc_args = Vec::with_capacity(raw_args.len());
    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];
        if arg == "--async-bugs-report" {
            let path = raw_args
                .get(i + 1)
                .ok_or_else(|| anyhow!("`--async-bugs-report` expects a path"))?;
            analyzer_options.json_report_path = Some(PathBuf::from(path));
            i += 2;
            continue;
        }
        if let Some(path) = arg.strip_prefix("--async-bugs-report=") {
            analyzer_options.json_report_path = Some(PathBuf::from(path));
            i += 1;
            continue;
        }
        rustc_args.push(arg.clone());
        i += 1;
    }

    if analyzer_options.json_report_path.is_none()
        && let Ok(path) = env::var("ASYNC_BUGS_REPORT_JSON")
        && !path.trim().is_empty()
    {
        analyzer_options.json_report_path = Some(PathBuf::from(path));
    }

    if !has_sysroot(&rustc_args) {
        let sysroot = detect_sysroot()?;
        rustc_args.push("--sysroot".to_string());
        rustc_args.push(sysroot);
    }

    let mut final_args = Vec::with_capacity(rustc_args.len() + 1);
    final_args.push("rustc".to_string());
    final_args.extend(rustc_args);
    Ok(DriverInput {
        rustc_args: final_args,
        analyzer_options,
    })
}

fn has_sysroot(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--sysroot" || arg.starts_with("--sysroot="))
}

fn looks_like_rustc_path(value: &str) -> bool {
    let p = Path::new(value);
    p.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name == "rustc")
}

fn detect_sysroot() -> Result<String> {
    let output = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .context("failed to execute `rustc --print sysroot`")?;
    if !output.status.success() {
        return Err(anyhow!("`rustc --print sysroot` exited with {}", output.status));
    }
    let sysroot = String::from_utf8(output.stdout).context("sysroot output was not utf-8")?;
    Ok(sysroot.trim().to_string())
}
