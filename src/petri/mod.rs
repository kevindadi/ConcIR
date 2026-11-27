//! Unified Petri-net execution module backed by [`crate::unipn_engine`].
//!
//! Legacy per-thread duplicated Petri-net files (`net.rs`, `exec.rs`) have been
//! replaced by [`crate::unipn_engine::UnipnEngine`] over [`unipn::hln`].

pub use crate::unipn_engine::{
    build, hln_to_val, val_to_hln, ControlToken, HlnNet as PetriNet, HlnState as NetState,
    LockHoldMode, LoweredHlnNet, PlaceKey, RwState, UnipnEngine as PetriEngine, WaitTag,
};

pub mod exec {
    pub use crate::unipn_engine::UnipnEngine as PetriEngine;
}

pub mod net {
    pub use crate::unipn_engine::{
        build, ControlToken, HlnNet as PetriNet, HlnState as NetState, LockHoldMode,
        LoweredHlnNet, PlaceKey, RwState, WaitTag,
    };
}
