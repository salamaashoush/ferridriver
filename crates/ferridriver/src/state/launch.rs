//! Browser instance launching, discovery, and connection helpers.

use std::sync::Arc;

use crate::backend::{AnyBrowser, BackendKind};
use crate::error::{FerriError, Result};
use rustc_hash::FxHashMap as HashMap;

use super::sessions::{
  chrome_flags_with, detect_firefox, detect_ladybird, detect_lightpanda, discover_chrome_ws, discover_ws_with_headers,
  resolve_chromium,
};
use super::{
  BrowserInstance, BrowserState, ConnectMode, ContextPages, InstanceOverridesFn, InstanceResolverFn, PendingOwner,
  allocation,
};

impl BrowserState {
  // ── Instance management ─────────────────────────────────────────────────

  /// Everything a launch needs from the state, snapshotted so the
  /// launch itself can run without holding the lock.
  pub(super) fn launch_spec(&self) -> LaunchSpec {
    LaunchSpec {
      backend_kind: self.backend_kind,
      browser_kind: self.browser_kind,
      launch_timeout: self.launch_timeout,
      connection_headers: self.connection_headers.clone(),
      launch_env: self.launch_env.clone(),
      firefox_user_prefs: self.firefox_user_prefs.clone(),
      chromium_sandbox: self.chromium_sandbox,
      headless: self.headless,
      chromium_path: self.chromium_path.clone(),
      user_data_dir: self.user_data_dir.clone(),
      connect_mode: self.connect_mode.clone(),
      base_args: self.extra_args.clone(),
      base_ignore_default_args: self.base_ignore_default_args.clone(),
      proxy: self.proxy.clone(),
      launched_proxies: Arc::clone(&self.launched_proxies),
      default_viewport: self.default_viewport.clone(),
      overrides_fn: self.instance_overrides_fn.clone(),
      resolver_fn: self.instance_resolver_fn.clone(),
    }
  }

  /// Per-instance launch permit. Two sessions racing to first-use the
  /// same instance must not each spawn a browser, and the loser has to
  /// wait for the winner rather than for the global write lock.
  pub(super) fn launch_permit(&self, instance: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
      map
        .entry(instance.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
  }

  /// Whether `instance` exists and its browser is still reachable.
  #[must_use]
  pub fn instance_is_live(&self, instance: &str) -> bool {
    self.instances.get(instance).is_some_and(|i| i.browser.is_alive())
  }

  /// Ensure `instance` is up, doing the launch off the state lock.
  ///
  /// [`Self::ensure_instance`] holds the caller's write guard across
  /// the whole launch — process spawn, protocol handshake, and any
  /// configured args/discovery subprocess — which stalls every other
  /// session in the server, including ones on a different browser. This
  /// takes the write lock to register ownership and install the finished instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or the
  /// connection fails.
  pub async fn ensure_instance_shared(state: &Arc<tokio::sync::RwLock<Self>>, instance: &str) -> Result<()> {
    let permit = {
      let guard = state.read().await;
      guard.ensure_not_disposed()?;
      if guard.instance_is_live(instance) {
        return Ok(());
      }
      guard.launch_permit(instance)
    };
    let held = Arc::clone(&permit).lock_owned().await;
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let allocation = {
      let mut guard = state.write().await;
      guard.ensure_not_disposed()?;
      if !Arc::ptr_eq(&permit, &guard.launch_permit(instance)) {
        return Err(allocation::Allocation::closed_error());
      }
      if guard.instance_is_live(instance) {
        return Ok(());
      }
      if guard.instances.contains_key(instance) {
        tracing::warn!(
          target: "ferridriver::state",
          instance,
          "browser instance is gone; relaunching",
        );
      }
      guard.check_cleanup_pending(instance)?;
      guard.prepare_allocation(instance)
    };
    allocation
      .spawn(&runtime, Arc::clone(state), instance.to_owned(), Some(held))
      .await
      .map_err(|error| FerriError::backend(format!("Browser initialization task stopped: {error}")))?
  }

  fn prepare_allocation(&mut self, instance: &str) -> allocation::Start {
    let (generation, owner, completion) = self.begin_allocation(instance);
    allocation::Start {
      spec: self.launch_spec(),
      shutdown_generation: self.shutdown_generation,
      generation,
      owner,
      completion,
    }
  }

  pub(crate) fn start_owned(
    mut self,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<impl std::future::Future<Output = Result<Arc<tokio::sync::RwLock<Self>>>> + Send + 'static> {
    self.ensure_not_disposed()?;
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let allocation = self.prepare_allocation("default");
    let state = self.into_shared();
    if let Some(resources) = resources {
      resources.retain(Arc::clone(&state))?;
    }
    let completed = allocation.spawn(&runtime, Arc::clone(&state), "default".into(), None);
    Ok(async move {
      completed
        .await
        .map_err(|error| FerriError::backend(format!("Browser initialization task stopped: {error}")))??;
      Ok(state)
    })
  }

  pub(crate) fn ensure_not_disposed(&self) -> Result<()> {
    if self.disposed {
      Err(FerriError::target_closed(Some(
        "browser resources have been disposed".into(),
      )))
    } else {
      Ok(())
    }
  }

  pub(super) fn begin_allocation(
    &mut self,
    instance: &str,
  ) -> (u64, Arc<allocation::Allocation>, allocation::Completion) {
    let generation = self.next_instance_generation();
    let (allocation, completion) = allocation::Allocation::new();
    self.pending_cleanup.push((
      instance.to_owned(),
      PendingOwner::Allocation {
        generation,
        owner: Arc::clone(&allocation),
      },
    ));
    (generation, allocation, completion)
  }

  pub(super) async fn finish_allocation(
    state: &Arc<tokio::sync::RwLock<Self>>,
    instance: &str,
    shutdown_generation: u64,
    generation: u64,
    allocation: &allocation::Allocation,
    created: Result<ConnectMode>,
  ) -> Result<()> {
    let mut guard = state.write().await;
    if guard.shutdown_generation != shutdown_generation || allocation.is_closing() {
      return Err(allocation::Allocation::closed_error());
    }
    Box::pin(guard.install_allocation(instance, generation, allocation, created)).await
  }

  pub(super) async fn install_allocation(
    &mut self,
    instance: &str,
    generation: u64,
    allocation: &allocation::Allocation,
    created: Result<ConnectMode>,
  ) -> Result<()> {
    let index = self.pending_instance_index(generation)?;
    let mode = match created {
      Ok(mode) => mode,
      Err(error) => {
        if !allocation.has_partial_resources() {
          self.pending_cleanup.swap_remove(index);
        }
        return Err(error);
      },
    };
    self.pending_cleanup[index].1 = BrowserInstance {
      browser: allocation.take_browser().await?,
      contexts: HashMap::default(),
      generation,
      refocus: crate::context::Refocus::default(),
    }
    .into();
    let install = Box::pin(self.finish_install_instance(
      instance,
      generation,
      !matches!(mode, ConnectMode::Launch),
      matches!(mode, ConnectMode::WebDriver { .. } | ConnectMode::Device { .. }),
    ));
    if let Some(budget) = allocation.connection_budget() {
      budget
        .scope(budget.wait(install))
        .await
        .map_err(|error| error.error("adopting browser pages"))?
    } else {
      install.await
    }
  }

  /// Evict a dead entry, adopt existing pages when connecting, and
  /// register `browser` as `instance_name`.
  pub(super) fn install_instance(
    &mut self,
    instance_name: &str,
    browser: AnyBrowser,
    adopt_pages: bool,
    preserve_viewport: bool,
  ) -> impl std::future::Future<Output = Result<()>> {
    let generation = self.next_instance_generation();
    // Registration must survive even when the returned future is never polled.
    self.pending_cleanup.push((
      instance_name.to_owned(),
      BrowserInstance {
        browser,
        contexts: HashMap::default(),
        generation,
        refocus: crate::context::Refocus::default(),
      }
      .into(),
    ));
    self.finish_install_instance(instance_name, generation, adopt_pages, preserve_viewport)
  }

  async fn finish_install_instance(
    &mut self,
    instance_name: &str,
    generation: u64,
    adopt_pages: bool,
    preserve_viewport: bool,
  ) -> Result<()> {
    if let Some(instance) = self.instances.remove(instance_name) {
      self.pending_cleanup.push((instance_name.to_owned(), instance.into()));
    }
    self
      .connected
      .store(!self.instances.is_empty(), std::sync::atomic::Ordering::Relaxed);
    self
      .cleanup_instances_except(Some(instance_name), Some(generation))
      .await?;
    if adopt_pages {
      let (viewport, defaults) = if preserve_viewport {
        (None, None)
      } else {
        (
          self.default_viewport.clone(),
          self
            .context_defaults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        )
      };
      let index = self.pending_instance_index(generation)?;
      let context_pages = self.context_pages.clone();
      let instance = self.pending_cleanup[index]
        .1
        .instance_mut()
        .ok_or_else(|| FerriError::backend("browser is not ready for adoption"))?;
      if let Err(error) = Self::adopt_instance_pages(
        instance_name,
        instance,
        viewport.as_ref(),
        defaults.as_ref(),
        &context_pages,
      )
      .await
      {
        return match self.cleanup_instances(Some(instance_name)).await {
          Ok(()) => Err(error),
          Err(cleanup) => Err(FerriError::backend(format!(
            "{error}; browser cleanup failed: {cleanup}"
          ))),
        };
      }
    }
    let index = self.pending_instance_index(generation)?;
    let (name, owner) = self.pending_cleanup.swap_remove(index);
    match owner {
      PendingOwner::Browser(instance) => {
        self.instances.insert(name, *instance);
        self.start_popup_pump(instance_name);
      },
      owner @ PendingOwner::Allocation { .. } => {
        self.pending_cleanup.push((name, owner));
        return Err(FerriError::backend("browser is not ready for installation"));
      },
    }
    self.connected.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }

  fn pending_instance_index(&self, generation: u64) -> Result<usize> {
    self
      .pending_cleanup
      .iter()
      .position(|(_, instance)| instance.generation() == generation)
      .ok_or_else(|| FerriError::backend("incoming browser disappeared during installation"))
  }

  async fn adopt_instance_pages(
    instance_name: &str,
    inst: &mut BrowserInstance,
    viewport: Option<&crate::options::ViewportConfig>,
    defaults: Option<&crate::options::BrowserContextOptions>,
    context_pages: &ContextPages,
  ) -> Result<()> {
    // Adopt existing pages into the "default" context of this instance.
    // For launch mode, pages are created on demand by the caller (the
    // test runner creates isolated contexts, MCP creates pages lazily).
    //
    // An adopted page is emulated with the configured viewport, exactly
    // as a page this instance opened would be. The browser on the other
    // end of a discover command is one this config declares and both
    // sides share, not a stranger's: a browser reached through a
    // persistent profile comes up at whatever window bounds Chrome
    // restored from the profile's last run, so adopting a page without
    // emulating anything hands every later call that stale size.
    // `viewport: null` is how a config asks to leave it alone; the
    // explicit `connect` tool ([`Self::connect_to_url`]) never touches
    // it, matching Playwright's `connectOverCDP`.
    //
    // A config with device or context options ([`Self::context_defaults`])
    // gets all of them, not just the viewport: they are the options of the
    // default context's pages. Only those pages are adopted: a page of a
    // context another client made is that client's, so it is neither
    // emulated, listed by `context.pages()`, nor taken over as a blank page.
    // Without such options a page still gets the media emulation every page
    // gets, as Playwright's `connectOverCDP` gives the pages it attaches to.
    let existing_pages = Box::pin(inst.browser.pages()).await?;
    let ctx = inst.context_mut("default");
    for page in existing_pages {
      page.attach_listeners(
        Arc::clone(&ctx.console_log),
        Arc::clone(&ctx.network_log),
        Arc::clone(&ctx.dialog_log),
      );
      ctx.pages.push(page);
    }
    context_pages.set(&format!("{instance_name}:default"), &ctx.pages);
    if let Some(options) = defaults {
      for page in &inst.context_mut("default").pages {
        // Boxed as `dyn`: the whole options path inlined into the launch
        // task's future overflows the compiler's `Send` check on it.
        let applied: std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> =
          Box::pin(page.apply_context_options(options));
        if let Err(e) = applied.await {
          tracing::warn!(
            target: "ferridriver::state",
            instance = instance_name,
            error = %e,
            "could not apply the configured context options to an adopted page",
          );
        }
      }
    } else {
      for page in &inst.context_mut("default").pages {
        if let Some(vp) = viewport
          && let Err(e) = page.emulate_viewport(vp).await
        {
          tracing::warn!(
            target: "ferridriver::state",
            instance = instance_name,
            error = %e,
            "could not emulate the configured viewport on an adopted page",
          );
        }
        if let Err(e) = page
          .emulate_context_media(crate::options::EmulateMediaOptions::default())
          .await
        {
          tracing::warn!(
            target: "ferridriver::state",
            instance = instance_name,
            error = %e,
            "could not emulate the default media features on an adopted page",
          );
        }
      }
    }
    Ok(())
  }

  pub(super) fn check_cleanup_pending(&self, name: &str) -> Result<()> {
    if self.pending_cleanup.iter().any(|(pending, _)| pending == name) {
      return Err(FerriError::backend(format!(
        "instance '{name}' has unfinished cleanup; retry closing it"
      )));
    }
    Ok(())
  }

  pub(super) async fn evict_instance(&mut self, instance_name: &str) -> Result<()> {
    if let Some(instance) = self.instances.remove(instance_name) {
      self.pending_cleanup.push((instance_name.to_owned(), instance.into()));
    }
    self.cleanup_instances(Some(instance_name)).await
  }

  pub(super) async fn cleanup_instances(&mut self, only: Option<&str>) -> Result<()> {
    self.cleanup_instances_except(only, None).await
  }

  async fn cleanup_instances_except(&mut self, only: Option<&str>, retained_generation: Option<u64>) -> Result<()> {
    let mut errors = Vec::new();
    let mut index = 0;
    while index < self.pending_cleanup.len() {
      let (name, instance) = &self.pending_cleanup[index];
      if only.is_some_and(|only| only != name) || retained_generation == Some(instance.generation()) {
        index += 1;
        continue;
      }
      let mut composites = Vec::new();
      if let Some(instance) = instance.instance() {
        composites.extend(instance.contexts.keys().map(|context| format!("{name}:{context}")));
        for context in instance.contexts.values() {
          for page in &context.pages {
            page.finish_screen_recording().await;
            page.dispose_local();
          }
        }
      }
      for composite in composites {
        self.purge_context_registries(&composite).await;
      }
      let (name, instance) = &mut self.pending_cleanup[index];
      let result = match instance {
        PendingOwner::Browser(instance) => {
          instance.contexts.clear();
          instance.browser.close().await
        },
        PendingOwner::Allocation { owner, .. } => owner.close().await,
      };
      match result {
        Ok(()) => {
          self.pending_cleanup.swap_remove(index);
        },
        Err(error) => {
          errors.push(format!("{name}: {error}"));
          index += 1;
        },
      }
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(FerriError::backend(errors.join("; ")))
    }
  }
}

/// What an instance was launched pointing at, by instance name.
pub(super) type LaunchedProxies = Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, LaunchedProxy>>>;

#[derive(Debug)]
pub(super) struct LaunchedProxy {
  pub(super) proxy: crate::options::ProxyConfig,
  /// Held, never read: the relay serves the browser until dropped.
  _relay: Option<Arc<crate::socks_relay::SocksRelay>>,
}

/// Immutable snapshot of the launch-relevant state, taken under a read
/// guard so the launch runs unlocked.
pub(super) struct LaunchSpec {
  pub(super) backend_kind: BackendKind,
  pub(super) browser_kind: crate::options::BrowserKind,
  pub(super) launch_timeout: Option<u64>,
  pub(super) connection_headers: Option<rustc_hash::FxHashMap<String, String>>,
  launch_env: rustc_hash::FxHashMap<String, String>,
  firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  chromium_sandbox: Option<bool>,
  headless: bool,
  chromium_path: String,
  user_data_dir: Option<String>,
  connect_mode: ConnectMode,
  base_args: Vec<String>,
  base_ignore_default_args: Option<crate::options::IgnoreDefaultArgs>,
  proxy: Option<crate::options::ProxyConfig>,
  launched_proxies: LaunchedProxies,
  default_viewport: Option<crate::options::ViewportConfig>,
  overrides_fn: Option<InstanceOverridesFn>,
  resolver_fn: Option<InstanceResolverFn>,
}

/// One instance's launch inputs after per-instance overrides are applied
/// to the state's base plan.
#[derive(Debug)]
pub(super) struct EffectiveLaunch {
  pub(super) firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  pub(super) chromium_sandbox: Option<bool>,
  pub(super) backend_kind: BackendKind,
  pub(super) browser_kind: crate::options::BrowserKind,
  pub(super) launch_timeout: Option<u64>,
  pub(super) headless: bool,
  pub(super) chromium_path: String,
  pub(super) user_data_dir: Option<String>,
  pub(super) args: Vec<String>,
  pub(super) env: rustc_hash::FxHashMap<String, String>,
  pub(super) ignore_default_args: Option<crate::options::IgnoreDefaultArgs>,
  pub(super) proxy: Option<crate::options::ProxyConfig>,
  /// What the browser ends up pointed at: `proxy`, or the proxy an
  /// instance's own flags name.
  pub(super) launched_proxy: Option<crate::options::ProxyConfig>,
}

impl EffectiveLaunch {
  /// Refuse `ignoreDefaultArgs` on a backend that has no default switch
  /// list to drop.
  ///
  /// Only the Chromium launch path injects a switch set
  /// ([`CHROMIUM_SWITCHES`]); the Firefox and `WebKit` paths pass just the
  /// structural transport flags (`--remote-debugging-port`, `--profile`,
  /// `--inspector-pipe`, `--user-data-dir`), which are not defaults a
  /// caller may drop without breaking the connection. Saying so is the
  /// point: accepting the option and applying it nowhere is how a caller
  /// believes a switch was dropped when it never was.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Unsupported`] naming the backend.
  fn reject_ignore_default_args(&self, backend: &str) -> Result<()> {
    if self.ignore_default_args.is_none() {
      return Ok(());
    }
    Err(FerriError::unsupported(format!(
      "ignoreDefaultArgs is not supported on the {backend} backend: ferridriver injects no default \
       switch list there, only the transport flags the connection needs. Drop the option, or set it \
       on the Chromium instances only."
    )))
  }
}

impl LaunchSpec {
  /// Resolve the connect mode and the effective launch inputs for
  /// `instance`.
  ///
  /// Both callbacks are operator-supplied and routinely spawn a
  /// subprocess or probe a TCP port, so they run on the blocking pool:
  /// on an async worker they stall unrelated tasks, and the args
  /// command for an operator-supplied gateway instance takes hundreds of ms.
  ///
  /// # Errors
  ///
  /// Propagates an overrides-callback error, which aborts the launch
  /// instead of silently starting an unconfigured browser.
  pub(super) async fn resolve_off_lock(&self, instance: &str) -> Result<(ConnectMode, EffectiveLaunch)> {
    let overrides_fn = self.overrides_fn.clone();
    let resolver_fn = self.resolver_fn.clone();
    let name = instance.to_string();

    // With no callbacks configured (the test runner, plain `launch()`)
    // there is nothing to run off-thread; skip the blocking-pool hop.
    let (overrides, mode) = if overrides_fn.is_none() && resolver_fn.is_none() {
      (crate::options::InstanceOverrides::default(), None)
    } else {
      let joined = tokio::task::spawn_blocking(move || {
        let overrides = overrides_fn.map_or_else(|| Ok(crate::options::InstanceOverrides::default()), |f| f(&name));
        let mode = resolver_fn.and_then(|f| f(&name));
        (overrides, mode)
      })
      .await;
      match joined {
        Ok((Ok(overrides), mode)) => (overrides, mode),
        Ok((Err(message), _)) => return Err(FerriError::backend(message)),
        Err(e) => {
          return Err(FerriError::backend(format!(
            "instance overrides/resolver task failed for '{instance}': {e}"
          )));
        },
      }
    };

    let mode = mode.unwrap_or_else(|| self.connect_mode.clone());
    let selection = crate::options::BrowserSelection::resolve(
      overrides
        .kind
        .or_else(|| overrides.backend.is_none().then_some(self.browser_kind)),
      overrides
        .backend
        .or_else(|| overrides.kind.is_none().then_some(self.backend_kind)),
    )?;
    let browser_kind = selection.browser;
    let headless = overrides.headless.unwrap_or(self.headless);
    let mut args = self.base_args.clone();
    args.extend(overrides.args);

    // Match the browser window to the viewport unless the caller
    // already pinned a size.
    if !matches!(&mode, ConnectMode::Device { .. })
      && !matches!(
        browser_kind,
        crate::options::BrowserKind::Safari
          | crate::options::BrowserKind::Ladybird
          | crate::options::BrowserKind::Lightpanda
      )
      && selection.backend != BackendKind::WebKit
      && !args.iter().any(|a| a.starts_with("--window-size"))
      && let Some(ref vp) = self.default_viewport
    {
      args.push(format!("--window-size={},{}", vp.width, vp.height));
    }

    let mut env = self.launch_env.clone();
    env.extend(overrides.env);
    let effective = EffectiveLaunch {
      firefox_user_prefs: self.firefox_user_prefs.clone(),
      chromium_sandbox: self.chromium_sandbox,
      backend_kind: selection.backend,
      browser_kind,
      launch_timeout: self.launch_timeout,
      headless,
      chromium_path: overrides.executable_path.unwrap_or_else(|| {
        if browser_kind == self.browser_kind {
          self.chromium_path.clone()
        } else {
          browser_executable(browser_kind, headless)
        }
      }),
      user_data_dir: overrides.user_data_dir.or_else(|| self.user_data_dir.clone()),
      args,
      env,
      ignore_default_args: overrides
        .ignore_default_args
        .or_else(|| self.base_ignore_default_args.clone()),
      proxy: self.proxy.clone(),
      launched_proxy: overrides.launched_proxy.or_else(|| self.proxy.clone()),
    };
    Ok((mode, effective))
  }

  pub(super) fn record_launched_proxy(
    &self,
    instance: &str,
    proxy: Option<&crate::options::ProxyConfig>,
    relay: Option<Arc<crate::socks_relay::SocksRelay>>,
  ) {
    let mut launched = self
      .launched_proxies
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match proxy {
      Some(proxy) => launched.insert(
        instance.to_string(),
        LaunchedProxy {
          proxy: proxy.clone(),
          _relay: relay,
        },
      ),
      None => launched.remove(instance),
    };
  }

  /// Point `eff` at a loopback relay when its proxy is an authenticated
  /// SOCKS5 one, which no browser can log in to. The relay has to outlive
  /// the launch, so it is handed back to be kept with the instance.
  pub(super) async fn relay_launch_proxy(
    &self,
    eff: &mut EffectiveLaunch,
  ) -> Result<Option<Arc<crate::socks_relay::SocksRelay>>> {
    let Some(proxy) = eff.proxy.as_ref().filter(|proxy| proxy.is_authenticated_socks()) else {
      return Ok(None);
    };
    let relay = crate::socks_relay::SocksRelay::start(proxy).await?;
    eff.proxy = Some(relay.browser_proxy(proxy));
    Ok(Some(relay))
  }

  pub(super) async fn launch_browser(&self, eff: &EffectiveLaunch) -> Result<AnyBrowser> {
    if let Some(browser) = Box::pin(launch_with_own_launcher(eff)).await? {
      return Ok(browser);
    }
    // `launch({ proxy })` is a per-process setting on every engine that has
    // one, so it is lowered here rather than at context creation: a context
    // that names no proxy of its own inherits it.
    let proxy_flags = eff
      .proxy
      .as_ref()
      .map(|proxy| proxy.launch_flags(eff.backend_kind))
      .unwrap_or_default();

    Ok(match eff.backend_kind {
      BackendKind::WebDriver => {
        return Err(FerriError::unsupported(
          "Local Classic WebDriver launching currently requires Safari; use a WebDriver endpoint for other installed drivers",
        ));
      },
      BackendKind::CdpPipe => {
        use crate::backend::cdp::CdpBrowser;
        let mut args = eff.args.clone();
        args.extend(proxy_flags);
        let flags = chrome_flags_with(
          eff.headless,
          &args,
          eff.ignore_default_args.as_ref(),
          eff.chromium_sandbox == Some(true),
        );
        let browser = match &eff.user_data_dir {
          Some(dir) => {
            CdpBrowser::launch_pipe_with_flags_in_dir(&eff.chromium_path, &flags, std::path::Path::new(dir), &eff.env)
              .await?
          },
          None => CdpBrowser::launch_pipe_with_flags(&eff.chromium_path, &flags, &eff.env).await?,
        };
        AnyBrowser::Cdp(browser)
      },
      BackendKind::CdpWs => {
        use crate::backend::cdp::CdpBrowser;
        let mut args = eff.args.clone();
        args.extend(proxy_flags);
        let flags = chrome_flags_with(
          eff.headless,
          &args,
          eff.ignore_default_args.as_ref(),
          eff.chromium_sandbox == Some(true),
        );
        let browser = match &eff.user_data_dir {
          Some(dir) => {
            Box::pin(CdpBrowser::launch_ws_with_flags_in_dir(
              &eff.chromium_path,
              &flags,
              std::path::Path::new(dir),
              &eff.env,
            ))
            .await?
          },
          None => CdpBrowser::launch_ws_with_flags(&eff.chromium_path, &flags, &eff.env).await?,
        };
        AnyBrowser::Cdp(browser)
      },
      BackendKind::WebKit => {
        use crate::backend::webkit::{LaunchConfig, WebKitBrowser};
        eff.reject_ignore_default_args("webkit")?;
        let config = LaunchConfig {
          executable_path: (!eff.chromium_path.is_empty()).then(|| std::path::PathBuf::from(&eff.chromium_path)),
          headless: eff.headless,
          env: eff.env.clone(),
          user_data_dir: eff.user_data_dir.as_ref().map(std::path::PathBuf::from),
          extra_args: eff.args.clone(),
          proxy_server: eff.proxy.as_ref().map(crate::options::ProxyConfig::server_origin),
          proxy_bypass_list: eff.proxy.as_ref().and_then(crate::options::ProxyConfig::bypass_list),
        };
        AnyBrowser::WebKit(Box::pin(WebKitBrowser::launch(&config)).await?)
      },
      BackendKind::Bidi => {
        use crate::backend::bidi::BidiBrowser;
        eff.reject_ignore_default_args("bidi")?;
        let mut flags = eff.args.clone();
        if eff.headless {
          flags.push("--headless".into());
        }
        // Firefox takes no proxy switch: the proxy is a session capability,
        // so it goes into `session.new` rather than onto the command line.
        AnyBrowser::Bidi(
          Box::pin(BidiBrowser::launch_with_flags(
            &eff.chromium_path,
            &flags,
            &eff.env,
            eff.user_data_dir.as_deref().map(std::path::Path::new),
            eff.proxy.as_ref(),
            eff.firefox_user_prefs.as_ref(),
          ))
          .await?,
        )
      },
    })
  }
}

/// Launch a product that has a launcher of its own rather than the
/// backend's browser spawn; `None` for every other product.
pub(super) async fn launch_with_own_launcher(eff: &EffectiveLaunch) -> Result<Option<AnyBrowser>> {
  Ok(Some(match eff.browser_kind {
    crate::options::BrowserKind::Safari => launch_safari(eff).await?,
    crate::options::BrowserKind::Ladybird => launch_ladybird(eff).await?,
    crate::options::BrowserKind::Lightpanda => launch_lightpanda(eff).await?,
    _ => return Ok(None),
  }))
}

pub(super) async fn launch_safari(eff: &EffectiveLaunch) -> Result<AnyBrowser> {
  eff.reject_ignore_default_args("safari")?;
  if eff.headless || eff.user_data_dir.is_some() || !eff.args.is_empty() || eff.proxy.is_some() {
    return Err(FerriError::unsupported(
      "Safari does not support headless mode, custom profiles, browser arguments, or a launch proxy",
    ));
  }
  if eff.backend_kind == BackendKind::WebDriver {
    Ok(AnyBrowser::WebDriver(
      Box::pin(crate::backend::webdriver::browser::WebDriverBrowser::launch_safari(
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  } else {
    Ok(AnyBrowser::Bidi(
      Box::pin(crate::backend::bidi::BidiBrowser::launch_safari(
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  }
}

/// Ladybird is launched through the `WebDriver` service it ships, which
/// builds the browser's command line itself: there is no channel for
/// browser arguments or a launch proxy.
pub(super) async fn launch_ladybird(eff: &EffectiveLaunch) -> Result<AnyBrowser> {
  eff.reject_ignore_default_args("ladybird")?;
  if !eff.args.is_empty() || eff.proxy.is_some() {
    return Err(FerriError::unsupported(
      "Ladybird's WebDriver service accepts no browser arguments or launch proxy",
    ));
  }
  // `chromium_path` is the resolved executable of whichever browser this is.
  let browser = if eff.chromium_path.is_empty() {
    std::path::PathBuf::from(detect_ladybird()?)
  } else {
    std::path::PathBuf::from(&eff.chromium_path)
  };
  let webdriver = crate::backend::webdriver::launcher::ladybird_webdriver(&browser)?;
  let user_data_dir = eff.user_data_dir.as_deref().map(std::path::Path::new);
  if eff.backend_kind == BackendKind::WebDriver {
    Ok(AnyBrowser::WebDriver(
      Box::pin(crate::backend::webdriver::browser::WebDriverBrowser::launch_ladybird(
        &webdriver,
        eff.headless,
        user_data_dir,
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  } else {
    Ok(AnyBrowser::Bidi(
      Box::pin(crate::backend::bidi::BidiBrowser::launch_ladybird(
        &webdriver,
        eff.headless,
        user_data_dir,
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  }
}

/// Lightpanda serves CDP itself; its arguments are `lightpanda serve`
/// options, and it has no window to show.
pub(super) async fn launch_lightpanda(eff: &EffectiveLaunch) -> Result<AnyBrowser> {
  eff.reject_ignore_default_args("lightpanda")?;
  if !eff.headless {
    return Err(FerriError::unsupported("Lightpanda is headless only"));
  }
  let binary = if eff.chromium_path.is_empty() {
    detect_lightpanda()?
  } else {
    eff.chromium_path.clone()
  };
  let browser = crate::backend::cdp::CdpBrowser::launch_lightpanda(
    std::path::Path::new(&binary),
    crate::backend::cdp::lightpanda::ServeOptions {
      proxy: eff.proxy.as_ref(),
      user_data_dir: eff.user_data_dir.as_deref().map(std::path::Path::new),
      args: &eff.args,
      env: &eff.env,
      timeout_ms: eff.launch_timeout.unwrap_or(30_000),
    },
  )
  .await?;
  Ok(AnyBrowser::Cdp(browser))
}

pub(super) fn browser_executable(kind: crate::options::BrowserKind, headless: bool) -> String {
  match kind {
    crate::options::BrowserKind::Chromium => resolve_chromium(headless),
    crate::options::BrowserKind::Firefox => std::env::var("FIREFOX_PATH")
      .ok()
      .or_else(|| detect_firefox().ok())
      .unwrap_or_else(|| "firefox".to_owned()),
    crate::options::BrowserKind::Safari => "/usr/bin/safaridriver".to_owned(),
    crate::options::BrowserKind::Ladybird => detect_ladybird().unwrap_or_default(),
    crate::options::BrowserKind::Lightpanda => detect_lightpanda().unwrap_or_default(),
    crate::options::BrowserKind::WebKit => String::new(),
  }
}

/// Ask a resolver about the full session key, then about the instance
/// half of a composite key (`"staging:default"` → `"staging"`).
pub(super) fn resolve_with_prefix(resolver: &InstanceResolverFn, instance_name: &str) -> Option<ConnectMode> {
  if let Some(mode) = resolver(instance_name) {
    return Some(mode);
  }
  let prefix = instance_name.split(':').next()?;
  if prefix == instance_name {
    return None;
  }
  resolver(prefix)
}

pub(super) async fn connect_classic(mode: &ConnectMode) -> Result<AnyBrowser> {
  let browser = match mode {
    ConnectMode::WebDriver {
      endpoint,
      browser_name,
      capabilities,
      headers,
      timeout,
      ..
    } => {
      crate::backend::webdriver::browser::WebDriverBrowser::connect(
        endpoint,
        browser_name,
        capabilities.as_ref(),
        headers.as_ref(),
        *timeout,
      )
      .await?
    },
    ConnectMode::ConnectUrl(endpoint) => {
      crate::backend::webdriver::browser::WebDriverBrowser::connect(endpoint, "safari", None, None, None).await?
    },
    ConnectMode::ConnectUrlWith { url, headers, timeout } => {
      crate::backend::webdriver::browser::WebDriverBrowser::connect(url, "safari", None, headers.as_ref(), *timeout)
        .await?
    },
    _ => {
      return Err(FerriError::invalid_argument(
        "endpoint",
        "Classic WebDriver requires an HTTP endpoint",
      ));
    },
  };
  Ok(AnyBrowser::WebDriver(browser))
}

/// Attach to a browser someone else is running. CDP uses discovery for HTTP
/// endpoints; `BiDi` accepts either its direct WebSocket endpoint or a W3C
/// `WebDriver` endpoint that negotiates `webSocketUrl` during session creation.
pub(super) async fn connect_browser(
  mode: &ConnectMode,
  backend_kind: BackendKind,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  args: &[String],
) -> Result<AnyBrowser> {
  use crate::backend::cdp::CdpBrowser;

  // An endpoint's own `connectOptions.headers` win over the state-wide ones.
  let merged;
  let headers = match mode {
    ConnectMode::ConnectUrlWith { headers: Some(own), .. } => {
      let mut all = headers.cloned().unwrap_or_default();
      all.extend(own.iter().map(|(name, value)| (name.clone(), value.clone())));
      merged = all;
      Some(&merged)
    },
    _ => headers,
  };
  if let ConnectMode::Device { target, headless } = mode {
    return match target {
      crate::device::DeviceTarget::Android(options) => crate::android::launch(options, *headless, args).await,
      crate::device::DeviceTarget::Ios(options) => Box::pin(crate::ios::launch(options, *headless)).await,
    };
  }
  if let ConnectMode::WebDriver {
    endpoint,
    browser_name,
    protocol,
    capabilities,
    headers,
    timeout,
  } = mode
  {
    return Box::pin(crate::backend::webdriver::connect(
      endpoint,
      browser_name,
      capabilities.as_ref(),
      headers.as_ref(),
      *timeout,
      *protocol,
    ))
    .await;
  }
  if backend_kind == BackendKind::WebDriver {
    return connect_classic(mode).await;
  }
  if backend_kind == BackendKind::Bidi {
    let endpoint = match mode {
      ConnectMode::ConnectUrl(endpoint) | ConnectMode::ConnectUrlWith { url: endpoint, .. }
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") =>
      {
        return Ok(AnyBrowser::Bidi(
          Box::pin(crate::backend::bidi::BidiBrowser::connect_webdriver(
            endpoint, "firefox", None, headers, None,
          ))
          .await?,
        ));
      },
      ConnectMode::ConnectUrl(endpoint) | ConnectMode::ConnectUrlWith { url: endpoint, .. } => endpoint,
      _ => return Err(FerriError::unsupported("WebDriver BiDi requires a WebSocket endpoint")),
    };
    return Ok(AnyBrowser::Bidi(
      Box::pin(crate::backend::bidi::BidiBrowser::connect_with_headers(
        endpoint, headers,
      ))
      .await?,
    ));
  }
  let ws_url = match mode {
    ConnectMode::ConnectUrl(url) | ConnectMode::ConnectUrlWith { url, .. }
      if url.starts_with("ws://") || url.starts_with("wss://") =>
    {
      url.clone()
    },
    ConnectMode::ConnectUrl(url) | ConnectMode::ConnectUrlWith { url, .. } => {
      discover_ws_with_headers(url, headers).await?
    },
    ConnectMode::AutoConnect { channel, user_data_dir } => discover_chrome_ws(channel, user_data_dir.as_deref())?,
    ConnectMode::Device { .. } | ConnectMode::WebDriver { .. } => {
      return Err(FerriError::unsupported(
        "WebDriver HTTP sessions require the BiDi backend",
      ));
    },
    ConnectMode::Launch => return Err(FerriError::backend("connect_browser called with Launch mode")),
  };
  Ok(AnyBrowser::Cdp(
    Box::pin(CdpBrowser::connect_with_headers(
      &ws_url,
      &headers
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect(),
    ))
    .await?,
  ))
}
