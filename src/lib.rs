//! Thin backward-compatible facade re-exporting the 7 `concir-*` workspace crates.

pub use concir_ir::{
    ast, diagnostic, env, export, expr, fqn, hash, schema, scir, summarize, typedef, validate,
};
pub use concir_sem as sem;
pub use concir_codegen as codegen;
pub use concir_engine::{graph, hln as unipn_engine, interp};

pub mod petri {
    pub use concir_engine::hln::{
        build, hln_to_val, val_to_hln, ControlToken, HlnNet as PetriNet, HlnState as NetState,
        LockHoldMode, LoweredHlnNet, PlaceKey, RwState, UnipnEngine as PetriEngine, WaitTag,
    };

    pub mod exec {
        pub use concir_engine::hln::UnipnEngine as PetriEngine;
    }

    pub mod net {
        pub use concir_engine::hln::{
            build, ControlToken, HlnNet as PetriNet, HlnState as NetState, LockHoldMode,
            LoweredHlnNet, PlaceKey, RwState, WaitTag,
        };
    }
}

pub use concir_verify::conform;
pub use concir_verify::model_check as explore;
pub use concir_verify::refinement;

pub mod design_refinement {
    pub use concir_verify::refinement::design::*;
}

pub mod req_ir {
    pub use concir_ir::req_ir::*;
    pub use concir_verify::req_check::*;
}

pub use concir_repair as repair;
