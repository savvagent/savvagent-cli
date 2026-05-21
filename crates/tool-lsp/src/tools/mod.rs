//! Per-tool implementations. Each module owns one MCP tool's input,
//! output, and dispatch shim. `LspServer` in `lib.rs` registers all of
//! them via rmcp's `tool_router`.

pub mod code_actions;
pub mod definition;
pub mod document_symbols;
pub mod hover;
pub mod references;
pub mod rename;
pub mod workspace_symbols;
// Keep this file as the single registration point.
