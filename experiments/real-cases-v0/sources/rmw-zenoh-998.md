# Source: ros2/rmw_zenoh issue #998

- URL: https://github.com/ros2/rmw_zenoh/issues/998
- Repository: https://github.com/ros2/rmw_zenoh
- License: Apache-2.0
- Retrieved: 2026-09-18
- Reported against: rolling @ e95c62d (v0.11.0); package ros-jazzy-rmw-zenoh-cpp 0.2.9-1noble
- State at retrieval: closed (no linked PR/commit shown)

## Mechanism (quoted/condensed from the issue's analysis)
"Classic ABBA deadlock on two mutexes: subscription `SubscriptionData::mutex_` and
the attached wait set's `rmw_wait_set_data_t::condition_mutex`."

Path A (`rmw_wait` -> `check_and_attach_condition`):
`rmw_zenoh.cpp:2240 unique_lock(condition_mutex)` -> `rmw_zenoh.cpp:2157` ->
`detail/rmw_subscription_data.cpp:916 lock(mutex_)`.
Order: condition_mutex -> mutex_.

Path B (`SubscriptionData::add_new_message`, zenoh RX callback):
`detail/rmw_subscription_data.cpp:1114 lock(mutex_)` -> `detail/event.cpp:263
lock(condition_mutex)`.
Order: mutex_ -> condition_mutex.

## gdb evidence (quoted from the issue)
"30792 holds WAITSET, wants SUB; 30744 holds SUB, wants WAITSET. True 2-cycle."

## Suggested fix direction (quoted/condensed)
"Release the subscription `mutex_` before acquiring the wait set
`condition_mutex` in `add_new_message` ... A consistent global lock order ...
is needed."

The full issue text and backtraces are at the URL above; this excerpt is kept
only for provenance.
