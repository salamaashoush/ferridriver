//! Script sources, from the v8 source rundown.
//!
//! Mirrors devtools-frontend `handlers/ScriptsHandler.ts`.
//!
//! Source text only reaches the trace when the
//! `disabled-by-default-devtools.v8-source-rundown-sources` category is
//! recorded, which [`crate::trace_categories`] does. A large script is
//! split across several `LargeScriptCatchup` events that have to be
//! concatenated in order, so the pieces are appended rather than
//! replacing one another.

use rustc_hash::FxHashMap;

use crate::event::TraceEvent;

/// The category that carries source text. `ScriptCatchup` also appears
/// under `v8-source-rundown` with metadata only, and treating that one
/// as a source event would blank the content.
const SOURCES_CATEGORY: &str = "disabled-by-default-devtools.v8-source-rundown-sources";

#[derive(Debug, Clone, Default)]
pub struct Script {
  pub url: String,
  pub content: String,
  pub is_module: bool,
}

/// Every script the trace carried source for, keyed by isolate and id
/// so two isolates using the same numeric id stay separate.
#[must_use]
pub fn from_events(events: &[TraceEvent<'_>]) -> Vec<Script> {
  let mut scripts: FxHashMap<(String, i64), Script> = FxHashMap::default();

  for event in events {
    if !matches!(
      event.name.as_ref(),
      "ScriptCatchup" | "LargeScriptCatchup" | "ScriptCompiled"
    ) {
      continue;
    }
    let Some(data) = event.data() else { continue };
    let isolate = data
      .get("isolate")
      .and_then(serde_json::Value::as_str)
      .unwrap_or_default()
      .to_string();
    let Some(script_id) = data.get("scriptId").and_then(serde_json::Value::as_i64) else {
      continue;
    };
    let entry = scripts.entry((isolate, script_id)).or_default();

    if let Some(url) = data.get("url").and_then(serde_json::Value::as_str)
      && !url.is_empty()
    {
      entry.url = url.to_string();
    }
    if let Some(is_module) = data.get("isModule").and_then(serde_json::Value::as_bool) {
      entry.is_module = is_module;
    }
    if event.cat == SOURCES_CATEGORY
      && let Some(text) = data.get("sourceText").and_then(serde_json::Value::as_str)
    {
      entry.content.push_str(text);
    }
  }

  scripts.into_values().filter(|s| !s.content.is_empty()).collect()
}
