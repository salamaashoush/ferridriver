//! `WebKitPage` file upload, interception, exposed functions, init scripts, and lifecycle.

use super::{
  CONTEXT_MENU_SUPPRESSOR, DESKTOP_ORIENTATION_SHIM, Ordering, Result, SAFARI_SURFACE_SHIM, WebKitPage, conn_err, json,
  protocol,
};

impl WebKitPage {
  // ── File upload / interception ────────────────────────────────────────

  /// Set the file list on a resolved `<input type=file>` element.
  /// Mirrors Playwright's `wkPage.ts::setInputFilePaths`, which pairs
  /// the target-session `DOM.setInputFiles` with a browser-session
  /// `Playwright.grantFileReadAccess` so the sandboxed `WebKit` process
  /// is allowed to read the payload bytes when the page later touches
  /// `input.files[i]`.
  ///
  /// # Errors
  ///
  /// Returns an error when the `Playwright.grantFileReadAccess` or `DOM.setInputFiles` command
  /// fails.
  pub async fn set_input_files_element(&self, element: &super::super::WebKitElement, paths: &[String]) -> Result<()> {
    let grant = self.browser.send(
      protocol::PLAYWRIGHT_GRANT_FILE_READ_ACCESS,
      json!({ "pageProxyId": &*self.proxy_id, "paths": paths }),
    );
    let set = element.set_input_files(paths);
    let (grant_result, set_result) = tokio::join!(grant, set);
    grant_result.map_err(conn_err)?;
    set_result
  }

  /// # Errors
  ///
  /// Returns an error when enabling interception on first use fails: the
  /// `Network.setInterceptionEnabled`, `Network.setResourceCachingDisabled` or
  /// `Network.addInterception` command.
  pub async fn route(&self, route: crate::route::RegisteredRoute) -> Result<()> {
    self.ensure_interception_enabled().await?;
    self.routes.write().await.push(route);
    Ok(())
  }

  async fn ensure_interception_enabled(&self) -> Result<()> {
    if self.intercept_enabled.swap(true, Ordering::SeqCst) {
      return Ok(());
    }
    let target = self.target_session();
    // A resource served from the page's memory cache is never requested,
    // so no route would see it. Pipeline all three setup calls in order.
    let (r1, r2, r3) = tokio::join!(
      target.send("Network.setInterceptionEnabled", json!({ "enabled": true })),
      target.send("Network.setResourceCachingDisabled", json!({ "disabled": true })),
      target.send(
        "Network.addInterception",
        json!({ "url": ".*", "stage": "request", "isRegex": true }),
      ),
    );
    r1.map_err(conn_err)?;
    r2.map_err(conn_err)?;
    r3.map_err(conn_err)?;
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature.
  pub async fn unroute(
    &self,
    matcher: &crate::url_matcher::UrlMatcher,
    scope: crate::route::RouteScope,
    handler_id: Option<usize>,
  ) -> Result<()> {
    self.routes.write().await.retain(|r| {
      r.scope != scope || !r.matcher.equivalent(matcher) || handler_id.is_some_and(|id| r.handler_id() != id)
    });
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature.
  pub async fn unroute_all(
    &self,
    _behavior: crate::options::UnrouteBehavior,
    scope: Option<crate::route::RouteScope>,
  ) -> Result<()> {
    let mut routes = self.routes.write().await;
    match scope {
      Some(scope) => routes.retain(|r| r.scope != scope),
      None => routes.clear(),
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; a failed `Page.setInterceptFileChooserDialog` is ignored and the `Result` matches
  /// the other backends' signature.
  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    let _ = self
      .target_session()
      .send("Page.setInterceptFileChooserDialog", json!({ "enabled": true }))
      .await;
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature.
  pub async fn enable_download_behavior(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  // ── Exposed functions / init scripts ──────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when installing the binding channel on first use fails (the
  /// `Runtime.addBinding` or `Page.setBootstrapScript` command, or evaluating the controller script
  /// with `Runtime.evaluate`), when `Page.setBootstrapScript` fails for the registration script, or
  /// when evaluating that script with `Runtime.evaluate` fails or throws.
  pub async fn expose_binding(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::backend::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.runtime_evaluate(&register_js, true).await?;
    self.evaluate_in_frame_contexts(&register_js).await;
    Ok(())
  }

  /// Run `expression` in every frame's document. An init script reaches
  /// only documents that load after it, so what a live child frame needs
  /// is evaluated there directly.
  async fn evaluate_in_frame_contexts(&self, expression: &str) {
    let contexts: Vec<i64> = self.frame_contexts.read().await.values().copied().collect();
    for context_id in contexts {
      let _ = self
        .target_session()
        .send(
          protocol::RUNTIME_EVALUATE,
          json!({ "expression": expression, "contextId": context_id }),
        )
        .await;
    }
  }

  /// [`Self::expose_binding`] for a popup target still held paused by
  /// `attach(defer_resume: true)` — paused-safe half only
  /// (`Runtime.addBinding` + init scripts); the live-document evaluates
  /// would block until `Target.resume`. The popup's first document runs
  /// the controller + registration at document start.
  pub(crate) async fn expose_binding_pre_doc(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    if !self.binding_initialized.swap(true, Ordering::SeqCst) {
      self
        .send_typed(&ferridriver_webkit_proto::runtime::AddBinding { name: "__fd_binding__" })
        .await?;
      self
        .add_init_script(crate::backend::common::BINDING_CONTROLLER_JS)
        .await?;
    }
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::backend::js_escape(name));
    self.add_init_script(&register_js).await?;
    // Best-effort live-document install, spawned so the paused target
    // cannot stall the pump — completes right after `Target.resume`
    // (the controller init script guards double-install).
    let this = self.clone();
    let controller_and_register = format!("{};{register_js}", crate::backend::common::BINDING_CONTROLLER_JS);
    tokio::spawn(async move {
      let _ = this.runtime_evaluate(&controller_and_register, true).await;
    });
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the page-side removal is best effort and the `Result` matches the other backends'
  /// signature.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::backend::js_escape(name)
    );
    let _ = self.runtime_evaluate(&js, true).await;
    self.evaluate_in_frame_contexts(&js).await;
    Ok(())
  }

  /// Install the `__fd_binding__` binding + `__fd_bc` controller JS on
  /// first use. `Runtime.bindingCalled` is dispatched by the page's main
  /// target listener (`events::handle_binding_called`), which follows
  /// cross-process target swaps — so exposed functions keep working after
  /// a navigation into a fresh process. `replay_binding_channel` re-arms
  /// the protocol side (`addBinding` + controller) on the swapped target.
  async fn ensure_binding_channel(&self) -> Result<()> {
    let _installing = self.binding_install.lock().await;
    if self.binding_initialized.swap(true, Ordering::SeqCst) {
      return Ok(());
    }
    self
      .send_typed(&ferridriver_webkit_proto::runtime::AddBinding { name: "__fd_binding__" })
      .await?;
    self
      .add_init_script(crate::backend::common::BINDING_CONTROLLER_JS)
      .await?;
    self
      .runtime_evaluate(crate::backend::common::BINDING_CONTROLLER_JS, true)
      .await?;
    self
      .evaluate_in_frame_contexts(crate::backend::common::BINDING_CONTROLLER_JS)
      .await;
    Ok(())
  }

  /// Register a binding the page reaches only through
  /// `__playwright__binding__controller__.callBinding`, never as a global.
  ///
  /// # Errors
  ///
  /// Returns an error when installing the binding channel on first use fails (the
  /// `Runtime.addBinding` or `Page.setBootstrapScript` command, or evaluating the controller script
  /// with `Runtime.evaluate`).
  pub async fn expose_callback(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  /// [`Self::expose_callback`] for a popup still held paused: the
  /// controller arrives with its first document's init scripts.
  pub(crate) async fn expose_callback_pre_doc(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    if !self.binding_initialized.swap(true, Ordering::SeqCst) {
      self
        .send_typed(&ferridriver_webkit_proto::runtime::AddBinding { name: "__fd_binding__" })
        .await?;
      self
        .add_init_script(crate::backend::common::BINDING_CONTROLLER_JS)
        .await?;
    }
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  pub async fn remove_callback(&self, name: &str) {
    self.exposed_fns.write().await.remove(name);
  }

  /// Whether the exposed-function binding channel has been initialised.
  /// Read by the provisional-target handler to decide whether to re-arm
  /// `Runtime.addBinding` on a freshly-swapped target.
  pub(crate) fn binding_channel_active(&self) -> bool {
    self.binding_initialized.load(Ordering::SeqCst)
  }

  /// The single bootstrap source: the built-in shims followed by every
  /// registered init script, in registration order.
  pub(crate) async fn bootstrap_source(&self) -> String {
    let mut fragments = vec![CONTEXT_MENU_SUPPRESSOR, SAFARI_SURFACE_SHIM];
    if !self.is_mobile_emulation() {
      fragments.push(DESKTOP_ORIENTATION_SHIM);
    }

    let scripts = self.init_scripts.lock().await;
    fragments
      .into_iter()
      .chain(scripts.iter().map(|(_, src)| src.as_str()))
      .collect::<Vec<_>>()
      .join(";\n")
  }

  /// # Errors
  ///
  /// Returns an error when the `Page.setBootstrapScript` command fails. The script stays registered
  /// and goes out with the next successful update.
  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    let id = self.init_script_seq.fetch_add(1, Ordering::Relaxed).to_string();
    {
      let mut scripts = self.init_scripts.lock().await;
      scripts.push((id.clone(), source.to_string()));
    }
    self.flush_bootstrap_script().await?;
    Ok(id)
  }

  /// # Errors
  ///
  /// Returns an error when the `Page.setBootstrapScript` command fails.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    {
      let mut scripts = self.init_scripts.lock().await;
      scripts.retain(|(id, _)| id != identifier);
    }
    self.flush_bootstrap_script().await?;
    Ok(())
  }

  /// Re-send `Page.setBootstrapScript` with the JOINED accumulation of
  /// every registered init-script fragment. `Page.setBootstrapScript`
  /// holds a single source that each call overwrites, so the only way
  /// to keep N scripts live across navigations is to concatenate them
  /// (mirrors `wkPage.ts::_calculateBootstrapScript`).
  pub(super) async fn flush_bootstrap_script(&self) -> Result<()> {
    let joined = self.bootstrap_source().await;
    self
      .send_typed(&ferridriver_webkit_proto::page::SetBootstrapScript { source: Some(&joined) })
      .await?;
    Ok(())
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Never fails; `Target.close` is best effort with a two-second limit, and the `Result` matches
  /// the other backends' signature.
  pub async fn close_page(&self, _opts: crate::options::PageCloseOptions) -> Result<()> {
    if self.closed.swap(true, Ordering::Relaxed) {
      return Ok(());
    }
    let target_id = self.target_id();
    let send = self.proxy.send(
      "Target.close",
      json!({ "targetId": target_id.to_string(), "runBeforeUnload": false }),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send).await;
    // Reject pending in-flight calls bound to this page's proxy + target
    // routes so successive callers don't pile up indefinitely.
    let conn = self.proxy.connection_handle();
    conn.close_route(Some(&self.proxy_id), Some(&target_id));
    conn.close_route(Some(&self.proxy_id), None);
    // CDP and BiDi both emit `Close` on their page emitter here —
    // without it `page.waitForEvent('close')`, the frame-cache
    // listener's exit, and the context-level `PageClose` bridge never
    // fire on WebKit.
    self.events.emit(crate::events::PageEvent::Close);
    Ok(())
  }

  /// Release this page's local resources without a protocol round trip —
  /// see the CDP `dispose_local` for why context teardown needs this.
  pub fn dispose_local(&self) {
    if self.closed.swap(true, Ordering::Relaxed) {
      return;
    }
    let conn = self.proxy.connection_handle();
    conn.close_route(Some(&self.proxy_id), Some(&self.target_id()));
    conn.close_route(Some(&self.proxy_id), None);
    super::super::worker::terminated(None, &self.workers);
    self.events.emit(crate::events::PageEvent::Close);
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Relaxed)
  }
}
