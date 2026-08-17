//! `Browser` -- NAPI binding for `ferridriver::Browser`.
//!
//! `Browser` instances are produced exclusively by the
//! [`crate::browser_type::BrowserType`] factory (`chromium()` /
//! `firefox()` / `webkit()` top-level functions). There is no
//! `Browser.launch` / `Browser.connect` static — that mirrors
//! Playwright's `chromium.launch()` / `firefox.launch()` /
//! `webkit.launch()` entry points.

use crate::error::IntoNapi;
use crate::page::Page;
use napi::Result;
use napi_derive::napi;

/// Browser instance. Manages contexts, pages, and browser lifecycle.
#[napi]
pub struct Browser {
  inner: ferridriver::Browser,
  /// `browser.on` / `browser.once` registrations, so `off(event,
  /// listener)` resolves the core `ListenerId` from JS function
  /// identity — the registry `Page` and `BrowserContext` also keep.
  listener_regs: std::sync::Arc<std::sync::Mutex<Vec<BrowserListenerReg>>>,
}

/// One browser-level listener registration.
struct BrowserListenerReg {
  event: String,
  id: u64,
  fn_ref: napi::bindgen_prelude::FunctionRef<BrowserContextArg, ()>,
}

impl Browser {
  /// Wrap a core Browser into a NAPI Browser.
  pub(crate) fn wrap(inner: ferridriver::Browser) -> Self {
    Self {
      inner,
      listener_regs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    }
  }

  /// Shared body of the registration methods: keep a `FunctionRef` for
  /// identity removal, then register on the core emitter.
  fn register_listener(
    &self,
    event: &str,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
    once: bool,
    front: bool,
  ) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let callback = build_browser_event_callback(listener)?;
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(BrowserListenerReg {
        event: event.to_string(),
        id: id.0,
        fn_ref,
      });
    Ok(())
  }
}

/// Cross-thread dispatch arg for `browser.on('context')` — carries the
/// live [`ferridriver::ContextRef`] across the tokio→napi boundary; the
/// `ToNapiValue` conversion (run on the JS thread) wraps it into the
/// [`crate::context::BrowserContext`] class instance.
pub struct BrowserContextArg(ferridriver::ContextRef);

impl napi::bindgen_prelude::ToNapiValue for BrowserContextArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    let wrapper = crate::context::BrowserContext::wrap(val.0);
    unsafe { crate::context::BrowserContext::to_napi_value(env, wrapper) }
  }
}

fn build_browser_event_callback(
  listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
) -> Result<ferridriver::events::BrowserEventCallback> {
  let tsfn = listener
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<true>()
    .max_queue_size::<0>()
    .build()?;
  Ok(std::sync::Arc::new(move |ev| match ev {
    ferridriver::events::BrowserEvent::Context(ctx) => {
      tsfn.call(
        BrowserContextArg(ctx),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    },
  }))
}

#[napi]
impl Browser {
  /// Create a new page (tab).
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Playwright: `browser.newBrowserCDPSession()`. Attaches a raw CDP
  /// session to the browser target. Chromium-only.
  #[napi(js_name = "newBrowserCDPSession")]
  pub async fn new_browser_cdp_session(&self) -> Result<crate::cdp_session::CDPSession> {
    let session = self.inner.new_browser_cdp_session().await.into_napi()?;
    Ok(crate::cdp_session::CDPSession::wrap(session))
  }

  /// Create a new page and navigate to URL.
  #[napi]
  pub async fn new_page_with_url(&self, url: String) -> Result<Page> {
    let page = Box::pin(self.inner.new_page_with_url(&url)).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Get the active page for the default context.
  #[napi]
  pub async fn page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext(options?)` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
  /// Every option field is optional; pass `undefined` or `{}` for
  /// no-options.
  ///
  /// The `ts_args_type` below forces the generated `.d.ts` to carry
  /// Playwright's exact string-literal unions (e.g. `colorScheme:
  /// 'light' | 'dark' | 'no-preference' | null`) — napi-rs's default
  /// inference would widen them to `string`.
  #[napi(ts_args_type = "options?: {
    acceptDownloads?: boolean;
    baseURL?: string;
    bypassCSP?: boolean;
    colorScheme?: 'light' | 'dark' | 'no-preference' | null;
    contrast?: 'no-preference' | 'more' | null;
    deviceScaleFactor?: number;
    extraHTTPHeaders?: Record<string, string>;
    forcedColors?: 'active' | 'none' | null;
    geolocation?: { latitude: number; longitude: number; accuracy?: number };
    hasTouch?: boolean;
    httpCredentials?: { username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' };
    ignoreHTTPSErrors?: boolean;
    isMobile?: boolean;
    javaScriptEnabled?: boolean;
    locale?: string;
    offline?: boolean;
    permissions?: string[];
    proxy?: { server: string; bypass?: string; username?: string; password?: string };
    recordVideo?: { dir: string; size?: { width: number; height: number } };
    reducedMotion?: 'reduce' | 'no-preference' | null;
    screen?: { width: number; height: number };
    serviceWorkers?: 'allow' | 'block';
    strictSelectors?: boolean;
    timezoneId?: string;
    userAgent?: string;
    viewport?: { width: number; height: number };
    /**
     * Set to `true` to opt out of viewport emulation entirely —
     * equivalent to Playwright's `viewport: null`. napi-rs cannot
     * distinguish JS `null` from `undefined`, so the opt-out is
     * exposed as this explicit boolean. Defaults to `false`.
     */
    disableViewport?: boolean;
  }")]
  pub fn new_context(
    &self,
    options: Option<crate::context::NapiBrowserContextOptions>,
  ) -> Result<crate::context::BrowserContext> {
    let core = options.map(crate::context::NapiBrowserContextOptions::into_core);
    // The core builder resolves synchronously (context registration is pure
    // bookkeeping); block_on keeps this method's sync JS shape.
    let ctx = napi::bindgen_prelude::block_on(std::future::IntoFuture::into_future(
      self.inner.new_context().maybe_options(core),
    ))
    .into_napi()?;
    Ok(crate::context::BrowserContext::wrap(ctx))
  }

  /// Get the default browser context.
  #[napi]
  pub fn default_context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner.default_context())
  }

  /// Register a browser-level event listener. Supports `'context'` —
  /// fired when a new context is created via [`Self::new_context`].
  /// Playwright: `browser.on('context', (context: BrowserContext) => …)`,
  /// which returns the browser so registrations chain.
  #[napi(
    ts_args_type = "event: 'context', listener: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  pub fn on<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, false, false)?;
    Ok(this.object)
  }

  /// Node's `addListener`, an alias of [`Self::on`].
  #[napi(
    ts_args_type = "event: 'context', listener: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  pub fn add_listener<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.on(this, event, listener)
  }

  /// One-shot variant of [`Self::on`].
  #[napi(
    ts_args_type = "event: 'context', listener: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  pub fn once<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, true, false)?;
    Ok(this.object)
  }

  /// Node's `prependListener`.
  #[napi(
    ts_args_type = "event: 'context', listener: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  pub fn prepend_listener<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, false, true)?;
    Ok(this.object)
  }

  /// Node's `prependOnceListener`.
  #[napi(
    ts_args_type = "event: 'context', listener: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  pub fn prepend_once_listener<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, true, true)?;
    Ok(this.object)
  }

  /// Remove a browser-level listener by function identity, Playwright's
  /// `off(event, listener)`; `off(event)` alone drops every listener for
  /// that event.
  #[napi(
    ts_args_type = "event: 'context', listener?: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off<'env>(
    &self,
    env: &napi::Env,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: Option<napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(listener) = listener else {
      self.inner.remove_listeners_named(&event);
      regs.retain(|r| r.event != event);
      return Ok(this.object);
    };
    let in_ref = listener.create_ref()?;
    let mut i = 0;
    while i < regs.len() {
      let hit = regs[i].event == event && {
        let a = in_ref.borrow_back(env)?;
        let b = regs[i].fn_ref.borrow_back(env)?;
        env.strict_equals(a, b)?
      };
      if hit {
        let reg = regs.remove(i);
        self.inner.off(ferridriver::events::ListenerId(reg.id));
      } else {
        i += 1;
      }
    }
    Ok(this.object)
  }

  /// Node's `removeListener`, an alias of [`Self::off`].
  #[napi(
    ts_args_type = "event: 'context', listener?: (context: BrowserContext) => void",
    ts_return_type = "this"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn remove_listener<'env>(
    &self,
    env: &napi::Env,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: Option<napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.off(env, this, event, listener)
  }

  /// Remove every browser-level listener, or only those for `event`.
  #[napi(ts_return_type = "this")]
  pub fn remove_all_listeners<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: Option<String>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match event {
      Some(ev) => {
        self.inner.remove_listeners_named(&ev);
        regs.retain(|r| r.event != ev);
      },
      None => {
        self.inner.remove_all_listeners();
        regs.clear();
      },
    }
    Ok(this.object)
  }

  /// Node's `listenerCount(type)`.
  #[napi]
  pub fn listener_count(&self, event: String) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let count = self.inner.listener_count(&event) as f64;
    count
  }

  /// Node's `eventNames()`.
  #[napi]
  pub fn event_names(&self) -> Vec<String> {
    self.inner.event_names()
  }

  /// Node's `setMaxListeners(n)`; `0` disables the leak warning.
  #[napi(ts_return_type = "this")]
  pub fn set_max_listeners<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    max: f64,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self
      .inner
      .set_max_listeners(crate::types::f64_to_u64(max.max(0.0)) as usize);
    Ok(this.object)
  }

  /// Node's `getMaxListeners()`.
  #[napi]
  pub fn get_max_listeners(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let max = self.inner.max_listeners() as f64;
    max
  }

  /// Wait for a browser-level event. Playwright:
  /// `browser.waitForEvent(event, options?)`. Supports `'context'`.
  #[napi(
    ts_args_type = "event: 'context', timeoutMs?: number",
    ts_return_type = "Promise<BrowserContext>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<crate::context::BrowserContext> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self.inner.wait_for_event(&event, timeout).await.into_napi()?;
    match ev {
      ferridriver::events::BrowserEvent::Context(ctx) => Ok(crate::context::BrowserContext::wrap(ctx)),
    }
  }

  /// Close the browser. Accepts Playwright's `{ reason? }` options shape;
  /// the reason is surfaced on `TargetClosed` errors emitted to
  /// in-flight operations on this browser's pages/contexts.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::BrowserCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::BrowserCloseOptions> = options.map(Into::into);
    self.inner.close().maybe_options(opts).await.into_napi()
  }

  /// List all browser contexts. Sync — mirrors Playwright's
  /// `browser.contexts(): BrowserContext[]`.
  #[napi]
  pub fn contexts(&self) -> Vec<crate::context::BrowserContext> {
    self
      .inner
      .contexts()
      .into_iter()
      .map(crate::context::BrowserContext::wrap)
      .collect()
  }

  /// Real product version string (e.g. `"HeadlessChrome/120.0.6099.109"`).
  #[napi(getter)]
  pub fn version(&self) -> String {
    self.inner.version().to_string()
  }

  /// Whether the browser is connected. Sync — mirrors Playwright's
  /// `browser.isConnected(): boolean`.
  #[napi]
  pub fn is_connected(&self) -> bool {
    self.inner.is_connected()
  }

  /// Publish this browser under a named session. Mirrors Playwright's
  /// `browser.bind(title, options): Promise<{ endpoint }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browser.ts:132`).
  ///
  /// Not available from this binding. A session's whole protocol is "run this
  /// script", so a host has to carry a script engine — and this addon is the
  /// core browser surface, deliberately without one. Rather than publish a
  /// registry entry no client can drive, this rejects and points at the two
  /// hosts that can: the CLI (`ferridriver session open`) and a ferridriver
  /// script (`browser.bind()` inside `ferridriver run`).
  #[napi(
    ts_args_type = "title: string, options?: {
    workspaceDir?: string;
    metadata?: Record<string, any>;
    host?: string;
    port?: number;
  }",
    ts_return_type = "Promise<{ endpoint: string }>"
  )]
  #[allow(clippy::unused_async)] // NAPI requires async to surface a JS Promise (Playwright parity)
  pub async fn bind(&self, title: String, options: Option<NapiBindOptions>) -> Result<BindResult> {
    let _ = (title, options);
    Err(napi::Error::from_reason(
      "browser.bind() is not available from the Node binding: a bound session runs scripts, and this addon \
       carries no script engine. Open the session with `ferridriver session open <id>`, or call browser.bind() \
       from a script run by `ferridriver run`.",
    ))
  }

  /// Stop accepting new connections for the bound session and remove its
  /// registry entry. Mirrors Playwright's `browser.unbind(): Promise<void>`.
  /// A no-op — [`Self::bind`] never binds from this addon — kept so code
  /// written against the Playwright shape still runs.
  #[napi]
  #[allow(clippy::unused_async)] // NAPI requires async to surface a JS Promise (Playwright parity)
  pub async fn unbind(&self) -> Result<()> {
    ferridriver_session::unbind_browser(&self.inner).map_err(|e| napi::Error::from_reason(e.to_string()))
  }
}

/// Options for [`Browser::bind`]. Field names mirror Playwright's option bag,
/// so code written against it still type-checks even though this addon's
/// `bind` refuses.
#[napi(object)]
#[derive(Default)]
pub struct NapiBindOptions {
  pub workspace_dir: Option<String>,
  pub metadata: Option<serde_json::Value>,
  pub host: Option<String>,
  pub port: Option<u32>,
}

/// The `{ endpoint }` object returned by [`Browser::bind`].
#[napi(object)]
pub struct BindResult {
  pub endpoint: String,
}
