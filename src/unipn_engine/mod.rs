//! Unified Petri-net execution engine backed by [`unipn::hln`].
//!
//! Lowering produces strictly **one subnet per function**, using [`HlnVal`] and
//! [`ControlToken`] colors to isolate per-thread stacks, local slots, held locks,
//! and typed wait states without duplicating subnets per thread or adding `Condvar`
//! resource places.

pub mod lower;
pub mod system;

pub use lower::{build, LoweredHlnNet, PlaceKey};
pub use system::{hln_to_val, val_to_hln, UnipnEngine};
pub use unipn::hln::{ControlToken, HlnFrame, HlnNet, HlnState, HlnVal, LockHoldMode, RwState, WaitTag};
