//! Component testing core (ct-core).
//!
//! Architecture mirrors Playwright CT:
//!
//! 1. **Import rewriting**: Component imports in test files become `ImportRef`
//!    descriptors (`{ type: 'importRef', id: 'src_Button_tsx' }`).
//!
//! 2. **Registry injection**: A Vite/Trunk plugin injects lazy `import()` calls
//!    for every referenced component into a browser-side registry.
//!
//! 3. **Mount via evaluate**: `mount()` serializes the component + props, sends
//!    them to the browser via `page.evaluate()`, which calls the framework's
//!    `window.__ferriMount(component, rootElement)`.
//!
//! 4. **Framework adapters**: Each framework provides a `registerSource` that
//!    implements `window.__ferriMount/Update/Unmount`.
//!
//! ## Rust (WASM) frameworks
//!
//! For Leptos/Dioxus/Yew, the flow is different — no Vite, no import rewriting.
//! The adapter crate provides a proc macro that generates the WASM entry point,
//! and `trunk serve` or `dx serve` handles building + serving.
//!
//! ## File layout
//!
//! ```text
//! ct/
//!   mod.rs        — this file (types + mount logic)
//!   server.rs     — ComponentServer (static file HTTP server)
//!   devserver.rs  — DevServer manager (spawns trunk/dx/vite, discovers URL)
//!   injected.js   — Browser-side registry + deserializer (injected into page)
//! ```

pub mod devserver;
pub mod server;

use std::collections::HashMap;

use crate::model::TestFailure;

/// A component reference — serialized form sent from test to browser.
/// The browser-side registry resolves this to the actual module via dynamic import().
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComponentRef {
  /// Import ID (e.g. "src_Counter_tsx" or "src_Counter_tsx_Counter").
  pub id: String,
  /// Props to pass to the component (JSON-serializable).
  #[serde(default)]
  pub props: serde_json::Value,
  /// Children (nested ComponentRefs or strings).
  #[serde(default)]
  pub children: Vec<serde_json::Value>,
}

/// Options passed to mount().
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MountOptions {
  /// Props for the component.
  #[serde(default)]
  pub props: serde_json::Value,
  /// Hook config passed to beforeMount/afterMount.
  #[serde(default, skip_serializing_if = "HashMap::is_empty")]
  pub hooks_config: HashMap<String, serde_json::Value>,
}

/// Mount a component in the browser.
///
/// This is the core mount operation. It:
/// 1. Navigates to the dev server URL (if not already there)
/// 2. Waits for the registry to be ready (`window.__ferriRegistry`)
/// 3. Calls `page.evaluate()` to invoke `window.__ferriMount(componentRef, rootElement)`
/// 4. Returns a locator pointing at the mounted component root
///
/// The framework adapter's `registerSource` must define `window.__ferriMount`.
pub async fn mount(
  page: &std::sync::Arc<ferridriver::Page>,
  _base_url: &str,
  component: &ComponentRef,
  options: &MountOptions,
) -> Result<ferridriver::Locator, TestFailure> {
  // Serialize component + options and send to browser.
  // The caller is responsible for navigating to the dev server URL first.
  let payload = serde_json::json!({
    "component": component,
    "options": options,
  });

  let escaped_json = payload.to_string().replace('\\', "\\\\").replace('`', "\\`");
  let js = format!(
    r#"(() => {{
      const data = JSON.parse(`{escaped_json}`);
      const root = document.getElementById('root') || document.getElementById('app');
      if (!root) throw new Error('No #root or #app element found');
      window.__ferriMount(data.component, root, data.options);
      return root.innerHTML;
    }})()"#,
  );

  let eval_result = page.evaluate(&js).await;
  eval_result.map_err(|e| TestFailure {
    message: format!("mount failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  })?;

  // Return a locator pointing at the component root.
  Ok(page.locator("#root, #app", None))
}

/// Unmount the currently mounted component.
pub async fn unmount(page: &ferridriver::Page) -> Result<(), TestFailure> {
  page
    .evaluate("() => { if (window.__ferriUnmount) window.__ferriUnmount(); }")
    .await
    .map_err(|e| TestFailure {
      message: format!("unmount failed: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
  Ok(())
}

/// The browser-side JavaScript that sets up the import registry.
/// Framework adapters append their `registerSource` after this.
pub const INJECTED_REGISTRY_JS: &str = r#"
// ferridriver CT: import registry + component deserializer.
window.__ferriRegistry = {};

window.__ferriRegister = function(id, importFn) {
  window.__ferriRegistry[id] = importFn;
};

// Resolve an importRef to the actual module.
window.__ferriResolve = async function(ref) {
  const loader = window.__ferriRegistry[ref.id];
  if (!loader) throw new Error(`Component not registered: ${ref.id}`);
  return await loader();
};
"#;
