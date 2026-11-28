//! ConcIR name resolution: language-neutral fully-qualified names.
//!
//! - Module identifier: `storage`, `core` (an identifier).
//! - Entity FQN: `module::entity` (exactly one `::`), e.g. `storage::log_mtx`.
//! - Control location: `module::function.sid`, e.g. `core::main.s3`.
//!
//! Same-module references use the short entity name. Cross-module references
//! must be FQNs and appear in the importing module's `requires`.

/// Separator between module and entity in an FQN.
pub const FQN_SEP: &str = "::";

/// True if `s` is a module or entity identifier: `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// `module::entity`.
pub fn fqn(module: &str, entity: &str) -> String {
    format!("{module}{FQN_SEP}{entity}")
}

/// Split `module::entity`. Rejects empty parts and extra `::`.
pub fn split_fqn(s: &str) -> Option<(&str, &str)> {
    let (module, entity) = s.split_once(FQN_SEP)?;
    if entity.contains(FQN_SEP) || !is_ident(module) || !is_ident(entity) {
        return None;
    }
    Some((module, entity))
}

pub fn is_fqn(s: &str) -> bool {
    split_fqn(s).is_some()
}

/// Control location `module::function.sid`.
pub fn location(module: &str, function: &str, sid: &str) -> String {
    format!("{module}{FQN_SEP}{function}.{sid}")
}

/// Split `module::function.sid`.
pub fn split_location(s: &str) -> Option<(&str, &str, &str)> {
    let (mod_fn, sid) = s.rsplit_once('.')?;
    let (module, function) = split_fqn(mod_fn)?;
    if sid.is_empty() {
        return None;
    }
    Some((module, function, sid))
}

/// Resolve `name` from `from_module`: FQN as-is, short name as `from_module::name`.
pub fn qualify(from_module: &str, name: &str) -> String {
    if is_fqn(name) {
        name.to_string()
    } else {
        fqn(from_module, name)
    }
}
