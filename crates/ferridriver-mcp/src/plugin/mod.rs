//! Plugin system for the MCP server.
//!
//! Each plugin is a single JavaScript/TypeScript file declaring a
//! manifest on `globalThis.exports`. At server startup every file is
//! rolldown-bundled + compiled to `QuickJS` bytecode once and its
//! manifest extracted, all in one batch runtime (see [`loader::load_all`]).
//!
//! Each session VM `Module::load`s the precompiled bytecode (no
//! per-session parse) and installs every declared handler as a binding
//! under `plugins.<name>(...)`. Optional tool promotion is handled by
//! the registry.

pub mod loader;
pub mod manifest;
pub mod registry;

pub use loader::{LoadedPlugin, PluginLoadError, discover, load_all};
pub use manifest::{PluginAllow, PluginManifest};
pub use registry::PluginRegistry;
