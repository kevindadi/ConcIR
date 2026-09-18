//! Round-2 review regressions (R1–R10). These encode the *correct* expected
//! outcomes, not the reviewed build's output.

use std::process::Command;

use concir::ast::Program;
use concir::explore::contract::{ContractError, ContractSpec};
use concir::explore::{verify_program, EngineKind};
use concir::repair::candidates::{FileCandidateProvider, LockOrderEnumerator};
use concir::repair::{run_repair, RepairOutcome};
use concir::sem::outcome::Outcome;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/repro_round2/{name}")).expect(name)
}

fn program(name: &str) -> Program {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn spec(name: &str) -> ContractSpec {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn run(
    program_name: &str,
    contract_name: &str,
    engine: EngineKind,
) -> concir::explore::VerificationReport {
    verify_program(&program(program_name), &spec(contract_name), engine)
}

fn both(program_name: &str, contract_name: &str) -> (Outcome, Outcome, bool, bool) {
    let i = run(program_name, contract_name, EngineKind::Interpreter);
    let p = run(program_name, contract_name, EngineKind::Petri);
    (i.outcome, p.outcome, i.complete, p.complete)
}

// ── R1: per-frame handle bindings ───────────────────────────────────

#[test]
fn r1_nested_handle_is_not_clobbered_by_a_call() {
    let (i, p, ic, pc) = both(
        "nested_handle_false_pass.json",
        "nested_handle_false_pass_contract.json",
    );
    assert_eq!(i, Outcome::Fail, "interpreter must find the deadlock");
    assert_eq!(p, Outcome::Fail, "petri must find the deadlock");
    assert!(ic && pc, "both searches must be complete");
}

#[test]
fn r1_alpha_renaming_callee_handle_does_not_change_result() {
    let a = run(
        "nested_handle_false_pass.json",
        "nested_handle_false_pass_contract.json",
        EngineKind::Interpreter,
    );
    let b = run(
        "nested_handle_renamed_control.json",
        "nested_handle_renamed_control_contract.json",
        EngineKind::Interpreter,
    );
    assert_eq!(a.outcome, Outcome::Fail);
    assert_eq!(b.outcome, Outcome::Fail);
}

#[test]
fn r1_control_without_gate_is_deadlock_free() {
    let (i, p, _, _) = both(
        "nested_handle_names.json",
        "nested_handle_names_contract.json",
    );
    assert_eq!(i, Outcome::Pass);
    assert_eq!(p, Outcome::Pass);
}

// ── R4: notify_one enumerates all waiters ───────────────────────────

#[test]
fn r4_notify_one_must_consider_every_waiter() {
    let (i, p, ic, pc) = both(
        "notify_choice_false_pass.json",
        "notify_choice_false_pass_contract.json",
    );
    assert_eq!(
        i,
        Outcome::Fail,
        "choosing w2 must be explored as a deadlock"
    );
    assert_eq!(p, Outcome::Fail);
    assert!(ic && pc);
}

// ── R5: notify_all without a static wait site ───────────────────────

#[test]
fn r5_notify_all_without_wait_advances() {
    let (i, p, ic, pc) = both(
        "notify_all_without_wait.json",
        "notify_all_without_wait_contract.json",
    );
    assert_eq!(i, Outcome::Pass);
    assert_eq!(
        p,
        Outcome::Pass,
        "the net must build a notify_all with no wait site"
    );
    assert!(ic && pc);
}

// ── R9: bodyless entry policy is unified ────────────────────────────

#[test]
fn r9_bodyless_entry_is_transparent_in_both_engines() {
    let (i, p, ic, pc) = both("bodyless_entry.json", "bodyless_entry_contract.json");
    assert_eq!(i, p, "bodyless entry outcomes must agree");
    assert_eq!(i, Outcome::Pass);
    assert!(ic && pc);
}

#[test]
fn r9_unused_unsupported_resource_does_not_block() {
    let (i, p, _, _) = both(
        "unused_unsupported.json",
        "unused_unsupported_contract.json",
    );
    assert_eq!(i, Outcome::Pass);
    assert_eq!(p, Outcome::Pass);
}

#[test]
fn r9_runtime_invalid_is_invalid_in_both_engines() {
    let (i, p, _, _) = both(
        "runtime_invalid_exit.json",
        "runtime_invalid_exit_contract.json",
    );
    assert_eq!(i, Outcome::Invalid);
    assert_eq!(p, Outcome::Invalid);
}

// ── R6: unified entry does static validation and rejects unsupported config ──

#[test]
fn r6_statically_invalid_program_is_invalid_not_pass() {
    let report = run(
        "invalid_protected_write_fixed_fixture.json",
        "invalid_protected_write_fixed_fixture_contract.json",
        EngineKind::Petri,
    );
    assert_eq!(report.outcome, Outcome::Invalid, "{:?}", report.invalid);
    assert!(!report.invalid.is_empty());
    // Both engines and the interpreter agree.
    let report_i = run(
        "invalid_protected_write_fixed_fixture.json",
        "invalid_protected_write_fixed_fixture_contract.json",
        EngineKind::Interpreter,
    );
    assert_eq!(report_i.outcome, Outcome::Invalid);
}

#[test]
fn r6_unsupported_assumptions_are_rejected() {
    let report = run(
        "ignored_assumptions.json",
        "ignored_assumptions_contract.json",
        EngineKind::Petri,
    );
    assert_eq!(
        report.outcome,
        Outcome::Unsupported,
        "{:?}",
        report.unsupported
    );
    assert!(!report.unsupported.is_empty());
}

#[test]
fn r6_contract_spec_errors_are_structured() {
    let p = program("ignored_assumptions.json");
    let sem = concir::sem::program::lower(&p).unwrap();
    // Unknown resource in a predicate -> Invalid, not a panic.
    let bad: ContractSpec = serde_json::from_str(
        r#"{"name":"bad","properties":[{"kind":"reachability","id":"g","goal":{"kind":"var_eq","resource":"nope","value":1}}]}"#,
    )
    .unwrap();
    assert!(matches!(bad.resolve(&sem), Err(ContractError::Invalid(_))));
    // Missing statement target -> Invalid.
    let missing: ContractSpec = serde_json::from_str(
        r#"{"name":"bad","properties":[{"kind":"reachability","id":"g","goal":{"kind":"statement_reached","function":"main::main","sid":"s99"}}]}"#,
    )
    .unwrap();
    assert!(matches!(
        missing.resolve(&sem),
        Err(ContractError::Invalid(_))
    ));
    // Zero bounds -> Invalid.
    let zero: ContractSpec = serde_json::from_str(
        r#"{"name":"bad","properties":[{"kind":"deadlock_free","id":"d"}],"bounds":{"max_states":0}}"#,
    )
    .unwrap();
    assert!(matches!(zero.resolve(&sem), Err(ContractError::Invalid(_))));
    // Mismatched predicate resource kind -> Invalid.
    let kind: ContractSpec = serde_json::from_str(
        r#"{"name":"bad","properties":[{"kind":"reachability","id":"g","goal":{"kind":"mutex_free","resource":"x"}}]}"#,
    )
    .unwrap();
    assert!(matches!(kind.resolve(&sem), Err(ContractError::Invalid(_))));
}

// ── R7: finite monitors ─────────────────────────────────────────────

#[test]
fn r7_finite_call_loop_is_complete_with_small_state_space() {
    let report = run(
        "finite_call_loop.json",
        "finite_call_loop_contract.json",
        EngineKind::Petri,
    );
    assert_eq!(report.outcome, Outcome::Pass, "{:?}", report.properties);
    assert!(report.complete);
    assert!(
        report.states_explored < 20,
        "monitor saturation should collapse the loop, got {} states",
        report.states_explored
    );
    let i = run(
        "finite_call_loop.json",
        "finite_call_loop_contract.json",
        EngineKind::Interpreter,
    );
    assert_eq!(i.outcome, Outcome::Pass);
    assert!(i.complete);
}

// ── R2/R3: repair permissions and contract rebinding ────────────────

fn repair_with(
    program_name: &str,
    contract_name: &str,
    patch_json: &str,
    budget: usize,
) -> concir::repair::RepairReport {
    let p = program(program_name);
    let s = spec(contract_name);
    let mut provider = FileCandidateProvider::from_json(patch_json).unwrap();
    run_repair(&p, &s, &mut provider, budget)
}

#[test]
fn r3_forbidden_delete_is_not_accepted() {
    let patch = fixture("delete_patch.json");
    let r = repair_with(
        "repair_original.json",
        "forbidden_delete_contract.json",
        &patch,
        4,
    );
    assert_ne!(r.outcome, RepairOutcome::Repaired);
    assert!(r
        .rounds
        .iter()
        .any(|x| x.reason.contains("not allowed by the contract")));
}

#[test]
fn r3_forbidden_reorder_is_not_accepted() {
    let patch = fixture("swap_patch.json");
    let r = repair_with(
        "repair_original.json",
        "forbidden_reorder_contract.json",
        &patch,
        4,
    );
    assert_ne!(r.outcome, RepairOutcome::Repaired);
    assert!(r
        .rounds
        .iter()
        .any(|x| x.reason.contains("not allowed by the contract")));
}

#[test]
fn r2_deleting_a_required_statement_is_not_accepted() {
    let patch = fixture("delete_patch.json");
    let r = repair_with(
        "repair_original.json",
        "stable_sid_contract.json",
        &patch,
        4,
    );
    assert_ne!(r.outcome, RepairOutcome::Repaired);
    // The contract requires main::t2.s3; after deletion the target is gone.
    assert!(r
        .rounds
        .iter()
        .any(|x| x.reason.contains("re-bound") || x.reason.contains("FAIL")));
}

#[test]
fn r2_fresh_resolve_of_deleted_target_is_invalid() {
    let report = run(
        "deleted_program.json",
        "stable_sid_contract.json",
        EngineKind::Petri,
    );
    assert_eq!(report.outcome, Outcome::Invalid, "{:?}", report.invalid);
    assert!(report.invalid.iter().any(|i| i.message.contains("s3")));
}

#[test]
fn r2_lock_order_repair_still_succeeds_when_allowed() {
    // The automatic enumerator (allow_lock_reorder = true) must still fix the
    // ABBA deadlock; permissions must not block legitimate repairs.
    let p = program("repair_original.json");
    let s = spec("forbidden_reorder_contract.json");
    // forbidden_reorder_contract disallows reorder, so no repair:
    let mut provider = LockOrderEnumerator::new(&p, &s.allowed_scope);
    let r = run_repair(&p, &s, &mut provider, 4);
    assert_ne!(r.outcome, RepairOutcome::Repaired);

    // With reorder allowed, it repairs.
    let s2: ContractSpec = serde_json::from_str(
        r#"{"name":"reorder","properties":[{"kind":"deadlock_free","id":"d"}],"allowed_scope":{"allow_lock_reorder":true}}"#,
    )
    .unwrap();
    let mut provider2 = LockOrderEnumerator::new(&p, &s2.allowed_scope);
    let r2 = run_repair(&p, &s2, &mut provider2, 4);
    assert_eq!(r2.outcome, RepairOutcome::Repaired, "{:?}", r2.rounds);
}

// ── R8: CLI exit codes ──────────────────────────────────────────────

fn cli_exit(args: &[&str]) -> i32 {
    let bin = env!("CARGO_BIN_EXE_concir-backend");
    Command::new(bin)
        .args(args)
        .output()
        .expect("run concir-backend")
        .status
        .code()
        .unwrap_or(-1)
}

#[test]
fn r8_cli_exit_codes_are_documented() {
    // PASS -> 0
    assert_eq!(
        cli_exit(&[
            "explore",
            "tests/repro_round2/nested_handle_names.json",
            "tests/repro_round2/nested_handle_names_contract.json",
        ]),
        0
    );
    // FAIL -> 1
    assert_eq!(
        cli_exit(&[
            "explore",
            "tests/repro_round2/nested_handle_false_pass.json",
            "tests/repro_round2/nested_handle_false_pass_contract.json",
        ]),
        1
    );
    // UNKNOWN -> 3
    assert_eq!(
        cli_exit(&[
            "explore",
            "tests/repro_round2/finite_call_loop.json",
            "tests/repro_round2/tiny_bounds_contract.json",
        ]),
        3
    );
    // INVALID -> 4
    assert_eq!(
        cli_exit(&[
            "explore",
            "tests/repro_round2/runtime_invalid_exit.json",
            "tests/repro_round2/runtime_invalid_exit_contract.json",
        ]),
        4
    );
    // UNSUPPORTED -> 5
    assert_eq!(
        cli_exit(&[
            "explore",
            "tests/repro_round2/ignored_assumptions.json",
            "tests/repro_round2/ignored_assumptions_contract.json",
        ]),
        5
    );
}
