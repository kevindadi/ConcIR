//! Strongly typed identifiers for the backend.
//!
//! Static identities (`ModuleId`, `ResourceId`, `FunctionId`, `StatementId`)
//! index into the resolved program. Dynamic identities (`ThreadId`,
//! `FrameId`, `ScopeId`, `HandleId`) are allocated from monotone counters so
//! that a reused slot always has a fresh identity.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident, $inner:ty) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub $inner);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(ModuleId, u32);
id_type!(ResourceId, u32);
id_type!(FunctionId, u32);
id_type!(StatementId, u32);
id_type!(ThreadId, u64);
id_type!(FrameId, u64);
id_type!(ScopeId, u64);
id_type!(HandleId, u64);

/// A frame-local slot index (params, locals, then the return slot).
pub type SlotId = usize;

/// Reference to a writable/readable slot in the operational store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SlotRef {
    /// A frame activation slot (param, local, or return slot).
    Local(SlotId),
    /// A shared `Var` or `Atomic` resource.
    Shared(ResourceId),
    /// `"_"`: discard a written value.
    Discard,
}
