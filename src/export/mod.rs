mod dot;

pub use dot::{DotDirection, DotOptions};

use crate::ast::{Function, Program};

impl Program {
    /// Export the entire program as a DOT digraph.
    pub fn to_dot(&self) -> String {
        self.to_dot_with_options(&DotOptions::default())
    }

    /// Export the entire program as a DOT digraph with custom options.
    pub fn to_dot_with_options(&self, options: &DotOptions) -> String {
        dot::program_to_dot(self, options)
    }
}

impl Function {
    /// Export a single function as a standalone DOT digraph.
    pub fn to_dot(&self) -> String {
        dot::function_to_dot(self)
    }
}
