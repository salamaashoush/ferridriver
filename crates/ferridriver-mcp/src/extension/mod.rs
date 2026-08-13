//! Extension system for the MCP server.
//!
//! Each extension is a JavaScript/TypeScript module that calls `tool(...)`.
//! At server startup every file is
//! rolldown-bundled + compiled to `QuickJS` bytecode once and its
//! manifest extracted, all in one batch runtime (see [`loader::load_all`]).
//!
//! Each session VM `Module::load`s the precompiled bytecode (no
//! per-session parse) and installs every declared handler under the
//! `tools` namespace. Optional MCP tool promotion is handled by the
//! registry.

pub mod loader;
pub mod manifest;
pub mod registry;
pub mod requirements;

pub use loader::{ExtensionLoadError, LoadedExtension, discover, discover_specs, load_all, resolve_specs};
pub use manifest::{ToolAllow, ToolManifest};
pub use registry::ExtensionRegistry;
pub use requirements::{RequirementEnv, RequirementIssue};
