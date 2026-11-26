//! Context-level exposed bindings and functions (applied across all pages).

use std::sync::Arc;

use crate::error::Result;

use super::{ContextRef, bind_source};

impl ContextRef {
  // ── Exposed bindings / functions (apply to all pages) ───────────────────

  /// Playwright: `browserContext.exposeBinding(name, callback)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:364`.
  ///
  /// Registers `window[name]` on every page in this context (current +
  /// future). The page-side call routes back into `callback`, which
  /// receives a [`crate::events::BindingSource`] as its first argument
  /// followed by the page-side call args. The callback's return value
  /// (after awaiting any returned promise in the binding layers) is
  /// delivered to the page-side caller.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the binding
  /// from the registry and from every page in the context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or injection fails
  /// on any page.
  pub async fn expose_binding(
    &self,
    name: &str,
    callback: crate::events::ExposedBinding,
  ) -> Result<crate::disposable::Disposable> {
    {
      let bindings = self.state.read().await.context_bindings_handle();
      let mut guard = bindings.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .insert(name.to_string(), Arc::clone(&callback));
    }
    // Apply to every page already open in this context.
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    let composite = self.key.to_composite();
    for page in &pages {
      let binding_for_page = bind_source(Arc::clone(&callback), composite.clone());
      page.expose_context_binding(name, binding_for_page).await?;
    }
    Ok(crate::disposable::Disposable::new({
      let this = self.clone();
      let name = name.to_string();
      move || async move {
        let _ = this.remove_exposed_binding(&name).await;
        Ok(())
      }
    }))
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the source argument:
  /// the supplied source-less [`crate::events::ExposedFn`] is wrapped
  /// into an [`crate::events::ExposedBinding`] that discards the
  /// [`crate::events::BindingSource`].
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or injection fails
  /// on any page.
  pub async fn expose_function(
    &self,
    name: &str,
    callback: crate::events::ExposedFn,
  ) -> Result<crate::disposable::Disposable> {
    let binding: crate::events::ExposedBinding = Arc::new(move |_source, args| callback(args));
    self.expose_binding(name, binding).await
  }

  /// Remove a previously exposed binding/function from the registry and
  /// from every page in this context. Mirrors Playwright's
  /// `BrowserContext.removeExposedBinding` (driven by `Disposable`).
  ///
  /// # Errors
  ///
  /// Returns an error if removal fails on any open page.
  pub async fn remove_exposed_binding(&self, name: &str) -> Result<()> {
    {
      let bindings = self.state.read().await.context_bindings_handle();
      let mut guard = bindings.write().await;
      if let Some(map) = guard.get_mut(&self.key.to_composite()) {
        map.remove(name);
      }
    }
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in &pages {
      page.remove_exposed_function(name).await?;
    }
    Ok(())
  }
}
