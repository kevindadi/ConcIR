# Mapping: ros2/rmw_zenoh #998 -> CIR

## Source mechanism (from the issue, not from this tool)
Classic ABBA on two `std::mutex`:
- Path A `rmw_wait`/`check_and_attach_condition`: waitset `condition_mutex` ->
  subscription `mutex_` (`rmw_zenoh.cpp:2240` then `.../rmw_subscription_data.cpp:916`).
- Path B `SubscriptionData::add_new_message` (zenoh RX callback): subscription
  `mutex_` -> waitset `condition_mutex` (`.../rmw_subscription_data.cpp:1114`,
  then `detail/event.cpp:263`).

## CIR correspondence
| source | CIR |
| --- | --- |
| executor thread `rmw_wait` | function `main::executor_wait` in the `main::main` scope |
| zenoh RX callback thread | function `main::rx_callback` in the same scope |
| wait set `condition_mutex` | resource `main::wait_set_condition_mutex` (sync Mutex) |
| subscription `mutex_` | resource `main::subscription_mutex` (sync Mutex) |
| Path A lock order | `executor_wait`: lock wait, lock sub, unlock sub, unlock wait |
| Path B lock order | `rx_callback`: lock sub, lock wait, unlock wait, unlock sub |
| `preserved` | both functions complete (reachability) |
| safety | `deadlock_free` |

## Omitted / abstracted
- All payload, queue, condvar, event and lifetime logic is omitted; only the two
  lock acquisitions and their release points are kept.
- The interval between the two locks is collapsed, so they are adjacent in the
  model. In the source they are separated by `check_and_attach_condition` /
  event-notification calls. This makes the model *more* susceptible to the
  adjacent-swap operator than faithful source-level reconstruction would be.
- `condition_variable.wait` releases `condition_mutex`; the model does not model
  condvars, so it over-approximates the window in which the waitset lock is held.
- Threads are spawned together and joined; scheduler fairness and priorities are
  not modelled.

## Ground truth
The defect (a genuine 2-cycle) comes from the upstream gdb evidence: "30792 holds
WAITSET, wants SUB; 30744 holds SUB, wants WAITSET". The reduction preserves the
two locks and their opposite acquisition order, so the deadlock is expected.

## Fix note
The issue's suggested fix (release `mutex_` before taking `condition_mutex`) is
*not* an adjacent lock swap and is not represented. The `fixed.cir.json` here is a
hypothesis model that unifies the lock order; whether the search can produce it
is tool evidence, not the upstream patch.
