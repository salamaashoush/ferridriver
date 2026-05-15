//! Plugin system for the MCP server.
//!
//! Each plugin is a single JavaScript file declaring a manifest on
//! `globalThis.exports`. At server startup the loader evaluates each plugin
//! in a throwaway `QuickJS` context to extract its metadata (name, description,
//! input schema, command allow-list). The full source text is retained.
//!
//! At `run_script` invocation time the per-call `QuickJS` context re-evaluates
//! each plugin's source and installs the resulting handler as a binding under
//! `plugins.<name>(...)`. Optional tool promotion is handled by the registry.

pub mod loader;
pub mod manifest;
pub mod registry;

pub use loader::{LoadedPlugin, PluginLoadError, load_plugin};
pub use manifest::{PluginAllow, PluginManifest};
pub use registry::PluginRegistry;
