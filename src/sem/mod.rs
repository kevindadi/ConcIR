//! Shared semantic foundations for the non-LLM backend.
//!
//! The resolved program and value domain are shared by the reference
//! interpreter and the Petri-net translation. Synchronization transitions are
//! implemented independently in each engine.

pub mod eval;
pub mod ids;
pub mod outcome;
pub mod program;
pub mod system;
pub mod value;
