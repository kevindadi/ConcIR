# CIR ↔ Reference Rust Consistency Checks

Each benchmark case carries both CIR and reference Rust; the two must be
consistent at the **statement level**; otherwise comparisons between external
baselines (Miri / Lockbud, which run Rust) and CVN (which runs CIR) lose meaning.
This document provides a correspondence table and an audit workflow; the same
table is also used to audit codegen outputs (verified CIR → generated Rust).

> The benchmark fixtures and experiment infrastructure that reference these
> constructs live in the downstream ConcPlanVerify repository (which consumes
> this crate as a submodule). Paths below are relative to that repo's root.

## 1. Construct Correspondence Table

| CIR construct | Reference Rust form | Consistency requirement |
| --- | --- | --- |
| `Mutex` resource | `Arc<Mutex<T>>` (cross-thread) or `Mutex<T>` | Exactly one Rust Mutex object per CIR Mutex |
| `Condvar` resource | `Arc<(Mutex<T>, Condvar)>` or a standalone `Condvar` | Paired mutex must match the CIR `wait` declaration |
| `Channel` resource | `std::sync::mpsc::channel/sync_channel` | Capacity semantics match (capacity 0 ↔ rendezvous) |
| `Semaphore` resource | Mutex+Condvar permit counter | Initial permit count matches CIR `init` |
| `Var` (protected) | Data under a lock (`T` of `Mutex<T>`) or fields accessed under a lock | May only be accessed while holding the protection-declared lock |
| `Atomic` | `std::sync::atomic::AtomicX` | Must not be degraded to an ordinary variable, nor upgraded in the reverse direction |
| Non-main function (closure) | **One independent** `thread::spawn` closure | One CIR function = one closure definition (must not fold isomorphic workers into a parameterized shared function; see §3) |
| `spawn f` / `join f` | `let h = thread::spawn(...)` / `h.join()` | Count and order match |
| `res_op l lock` / `drop` | Guard scope boundaries; early release via explicit `drop(guard)` | **Acquisition order matches statement-by-statement** (the essence of lock-order cases) |
| `res_op cv wait mtx` | `while !pred { guard = cv.wait(guard) }` | Loop re-check form; paired mutex matches |
| `notify` / `notify_all` | `cv.notify_one()` / `cv.notify_all()` | One-shot vs broadcast must not be swapped |
| `send` / `recv` | `tx.send(..)` / `rx.recv()` | Blocking semantics match |
| `write v` / `read v` | Assignment / read under a lock | Written constant values match (goals depend on concrete values) |
| `branch` transfer | `if`/`else`, same condition, same shared variables | Branch structure 1:1; must not merge or eliminate |
| `cas` | `compare_exchange`, branching on the **return value** | True arm = CAS success arm (LLM judges have misread this) |
| `goals` | Trailing `assert!` (approximation visible to dynamic baselines) | Predicates encoded by asserts match goal predicates |

## 2. Audit Workflow (Once per Case)

1. **Function inventory**: CIR `functions` correspond 1:1 with Rust `main` plus each spawn closure.
2. **Statement walkthrough**: For each sid in order, find the corresponding line in Rust and record `sid ↔ line number`;
   Focus on the **lock acquisition sequence** (names and order) and **branch conditions** within each function.
3. **Resource inventory**: CIR resources correspond 1:1 with Rust objects; protection relationships appear as
   "data under the lock" or comment declarations.
4. **Defect essence**: The difference between buggy and fixed must equal exactly the defect described in the manifest `notes`
   (e.g. deep_lock_chain_4x3: only the m2/m1 order on w3's else arm differs).
5. **goals**: CIR goal predicates match trailing Rust asserts (if any).

## 3. Known Deviation Categories

| Category | Verdict | Notes |
| --- | --- | --- |
| Codegen folds isomorphic workers into a parameterized shared function | ⚠ Semantically equivalent but not statement-level 1:1 | Observed in codegen experiments; row 7 of the correspondence table explicitly forbids this; may later become a codegen prompt constraint or acceptance check |
| `Var` → `Atomic` drift during repair experiments | ✗ Violates resource correspondence | Invisible to the repair oracle; local regeneration (`repair_local`) structurally eliminates this by freezing the non-slice portion |
| Reference Rust approximates EF goals with `assert!` | ⚠ Accepted | Dynamic tools can only observe terminal states; EF (reachable under some schedule) cannot be asserted directly; note this in comments |
