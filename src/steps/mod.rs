//! BDD step definitions — trait-based registry with self-documenting steps.
//!
//! Each step is a self-contained unit: pattern + description + example + handler.
//! Adding a new step = define with `step!()` macro + register in the module's `register()`.

use async_trait::async_trait;
use chromiumoxide::Page;
use regex::Regex;
use std::collections::HashMap;

#[macro_use]
mod macros;
mod registry;

pub mod assertion;
pub mod cookie;
pub mod interaction;
pub mod javascript;
pub mod navigation;
pub mod screenshot;
pub mod storage;
pub mod variable;
pub mod wait;

pub use registry::StepRegistry;

/// Every step implements this trait.
#[async_trait]
pub trait StepDef: Send + Sync {
    fn description(&self) -> &'static str;
    fn category(&self) -> StepCategory;
    fn example(&self) -> &'static str;
    fn pattern(&self) -> &Regex;

    async fn execute(
        &self,
        page: &Page,
        caps: &regex::Captures<'_>,
        data_table: Option<&[Vec<String>]>,
        vars: &mut HashMap<String, String>,
    ) -> Result<Option<serde_json::Value>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum StepCategory {
    Navigation,
    Interaction,
    Wait,
    Assertion,
    Variable,
    Cookie,
    Storage,
    Screenshot,
    JavaScript,
}

/// Extract a quoted or bare string from a regex capture.
pub fn q(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Escape a string for use in JS string literals.
pub fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
