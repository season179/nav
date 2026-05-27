//! Typed, permissioned, observable, and recoverable tool access.

#[derive(Debug, Default)]
pub struct ToolRegistry;

impl ToolRegistry {
    /// Returns the names of all registered tools, sorted for deterministic
    /// output. Empty until TOOL-01 lands.
    pub fn tool_names(&self) -> Vec<&str> {
        vec![]
    }
}
