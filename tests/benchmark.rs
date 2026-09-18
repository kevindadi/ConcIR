//! Development benchmark: independent expectations and machine-readable
//! records for the three repair strategies.

use concir::repair::benchmark::{check_benchmark, export_and_reverify, run_benchmark};
use concir::repair::search::RepairStrategy;

#[test]
fn dev_benchmark_expectations_hold() {
    if let Err(e) = check_benchmark() {
        panic!("{e}");
    }
}

#[test]
fn repaired_case_exports_and_reverifies() {
    for case in ["single_cycle", "two_cycles", "cross_module_two_cycles"] {
        for strategy in [RepairStrategy::Composite, RepairStrategy::Diagnostic] {
            let ok = export_and_reverify(case, strategy)
                .unwrap_or_else(|| panic!("{case}/{:?} produced no accepted program", strategy));
            assert!(
                ok,
                "{case}/{:?}: exported program did not re-verify",
                strategy
            );
        }
    }
}

#[test]
fn benchmark_records_are_deterministic() {
    let a = run_benchmark(&[RepairStrategy::Diagnostic]);
    let b = run_benchmark(&[RepairStrategy::Diagnostic]);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.case, y.case);
        assert_eq!(x.outcome, y.outcome, "{}", x.case);
        assert_eq!(x.stop_reason, y.stop_reason, "{}", x.case);
        assert_eq!(x.candidates_tried, y.candidates_tried, "{}", x.case);
        assert_eq!(x.verifications, y.verifications, "{}", x.case);
        assert_eq!(x.patch_size, y.patch_size, "{}", x.case);
    }
}

#[test]
fn single_baseline_still_solves_single_cycle_only() {
    let records = run_benchmark(&[RepairStrategy::Single]);
    let single = records.iter().find(|r| r.case == "single_cycle").unwrap();
    assert!(single.reached_expected);
    let two = records.iter().find(|r| r.case == "two_cycles").unwrap();
    assert!(!two.reached_expected);
    assert_ne!(two.outcome, concir::repair::RepairOutcome::Repaired);
}
