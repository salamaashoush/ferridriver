//! The tools a page offers about itself.
//!
//! A page can answer a `devtoolstooldiscovery` event with a group of
//! named callables, each with a JSON schema for its input. An agent
//! then has the site's own vocabulary rather than only the DOM: `add to
//! cart` instead of clicking the third button in the second row.
//! `chrome-devtools-mcp` spends two tools on this
//! (`list_3p_developer_tools`, `execute_3p_developer_tool`).
//!
//! # No protocol, so no backend to be unsupported on
//!
//! It is a DOM event and a function call, so this works the same on
//! every backend. Upstream first asks CDP's `DOMDebugger.getEventListeners`
//! whether the page has a `devtoolstooldiscovery` listener at all, and
//! skips the dispatch if not. That is a Chromium-only shortcut around a
//! dispatch that is already cheap, and skipping it is what makes this
//! answer the same on Firefox and `WebKit`.

use serde::{Deserialize, Serialize};

use crate::error::{FerriError, Result};

/// One group of tools, as a page announced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageToolGroup {
  pub name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  pub tools: Vec<PageTool>,
}

/// One callable the page exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageTool {
  pub name: String,
  pub description: String,
  /// The JSON schema its input has to satisfy. Whatever the page wrote,
  /// unread by us: it is the page's contract with its caller.
  pub input_schema: serde_json::Value,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub annotations: Option<serde_json::Value>,
}

/// Dispatch the discovery event and collect what answers.
#[must_use]
pub fn discover_source() -> &'static str {
  DISCOVER
}

/// Call one of them.
#[must_use]
pub fn execute_source() -> &'static str {
  EXECUTE
}

/// # Errors
///
/// [`FerriError::Backend`] when the page returned something other than
/// the shape it was asked for.
pub fn parse_groups(payload: &str) -> Result<Vec<PageToolGroup>> {
  serde_json::from_str(payload).map_err(|e| FerriError::backend(format!("page tool groups were unreadable: {e}")))
}

/// # Errors
///
/// [`FerriError::Backend`] when the page returned something other than
/// the shape it was asked for, and the tool's own error where it threw.
pub fn parse_result(payload: &str) -> Result<serde_json::Value> {
  let value: serde_json::Value =
    serde_json::from_str(payload).map_err(|e| FerriError::backend(format!("page tool result was unreadable: {e}")))?;
  if let Some(message) = value.get("error").and_then(serde_json::Value::as_str) {
    return Err(FerriError::backend(format!("page tool failed: {message}")));
  }
  Ok(value.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

/// The dispatch, and the `window.__dtmcp` surface it leaves behind.
///
/// The shape is upstream's, down to the validation and the one-macrotask
/// grace period for a listener that answers asynchronously, because a
/// page written against `chrome-devtools-mcp` has to work here
/// unchanged: it is the page that decides what `respondWith` is called
/// with, and a second definition of that contract would fork it.
const DISCOVER: &str = r"
(async () => {
  // A second discovery must not accumulate the first one's groups.
  if (window.__dtmcp) window.__dtmcp.toolGroups = [];

  const groups = await new Promise((resolve) => {
    const event = new CustomEvent('devtoolstooldiscovery');
    const collected = [];
    event.respondWith = (toolGroup) => {
      if (!window.__dtmcp) window.__dtmcp = {};
      if (!window.__dtmcp.toolGroups) window.__dtmcp.toolGroups = [];
      if (typeof toolGroup.name !== 'string'
        || (toolGroup.description && typeof toolGroup.description !== 'string')
        || !Array.isArray(toolGroup.tools)) {
        return;
      }
      for (const tool of toolGroup.tools) {
        if (typeof tool.name !== 'string'
          || typeof tool.description !== 'string'
          || typeof tool.inputSchema !== 'object'
          || typeof tool.execute !== 'function') {
          return;
        }
      }
      window.__dtmcp.toolGroups.push(toolGroup);
      if (!window.__dtmcp.executeTool) {
        window.__dtmcp.executeTool = async (toolName, args) => {
          if (!window.__dtmcp?.toolGroups || window.__dtmcp.toolGroups.length === 0) {
            throw new Error('No tools found on the page');
          }
          for (const group of window.__dtmcp.toolGroups) {
            const tool = group.tools?.find((t) => t.name === toolName);
            if (tool) return await tool.execute(args);
          }
          throw new Error('Tool ' + toolName + ' not found');
        };
      }
      collected.push(toolGroup);
    };
    window.dispatchEvent(event);
    // A listener that answers synchronously is done; one that answers
    // from a microtask gets until the next macrotask and no longer.
    if (collected.length > 0) resolve(collected);
    else setTimeout(() => resolve(collected), 0);
  });

  return JSON.stringify(groups.map((group) => ({
    name: group.name,
    description: typeof group.description === 'string' ? group.description : undefined,
    tools: (group.tools ?? []).map((tool) => ({
      name: tool.name,
      description: tool.description,
      inputSchema: tool.inputSchema,
      annotations: tool.annotations ?? undefined,
    })),
  })));
})
";

/// Call the tool and make what it returned safe to send over the wire.
///
/// A tool may hand back anything JavaScript holds, and most of it does
/// not survive `JSON.stringify`: a DOM element serialises as `{}`, a
/// cycle throws, a function disappears. Upstream walks the result and
/// replaces each of those with something a reader can act on, and the
/// element case is the one worth knowing about -- the element itself is
/// parked on `window.__dtmcp.stashedElements`, so the id in its place
/// is a live handle rather than a description of one. That array is
/// never cleared, which is what makes a `stashedId` still resolve after
/// the next call; upstream empties it because it adopts each element
/// into a snapshot on the way out.
const EXECUTE: &str = r"
(async (call) => {
  if (!window.__dtmcp?.executeTool) {
    return JSON.stringify({ error: 'this page exposes no developer tools' });
  }

  const stash = (el) => {
    if (!window.__dtmcp.stashedElements) window.__dtmcp.stashedElements = [];
    window.__dtmcp.stashedElements.push(el);
    return { stashedId: 'stashed-' + (window.__dtmcp.stashedElements.length - 1) };
  };

  const ancestors = [];
  const process = (data, parent) => {
    if (data instanceof Element) return stash(data);
    if (Array.isArray(data)) return data.map((item) => process(item, parent));
    if (data !== null && typeof data === 'object') {
      while (ancestors.length > 0 && ancestors.at(-1) !== parent) ancestors.pop();
      if (ancestors.includes(data)) return '<Circular reference>';
      ancestors.push(data);
      // Anything with a prototype of its own is named rather than
      // walked: a Map, a Date and a class instance all serialise as
      // `{}` and would read as an empty result.
      const proto = Object.getPrototypeOf(data);
      if (proto !== Object.prototype) {
        return '<' + (proto === null ? 'null-prototype' : (data.constructor?.name ?? 'anonymous')) + ' instance>';
      }
      const out = {};
      for (const [key, value] of Object.entries(data)) out[key] = process(value, data);
      return out;
    }
    if (typeof data === 'function') return '<Function object>';
    return data;
  };

  try {
    const result = await window.__dtmcp.executeTool(call.name, call.params ?? {});
    return JSON.stringify({ result: process(result, null) });
  } catch (e) {
    return JSON.stringify({ error: e instanceof Error ? e.message : String(e) });
  }
})
";
