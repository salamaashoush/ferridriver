//! Browser management -- mirrors Playwright's `Browser` interface.
//!
//! `Browser` instances are produced by the [`crate::BrowserType`]
//! factory ([`crate::chromium`] / [`crate::firefox`] /
//! [`crate::webkit`]) — there is no `Browser::launch` /
//! `Browser::connect` shortcut. This matches Playwright's
//! `chromium.launch()` / `firefox.launch()` / `webkit.launch()` entry
//! points.
//!
//! ```ignore
//! use ferridriver::{chromium, options::LaunchOptions};
//!
//! let browser = chromium().launch(LaunchOptions::default()).await?;
//! let page = browser.new_page_with_url("https://example.com").await?;
//! ```

use crate::context::ContextRef;
use crate::error::Result;
use crate::page::Page;
use crate::state::BrowserState;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Browser instance. Manages contexts, pages, and browser lifecycle.
///
/// `Clone` is cheap — all clones share the same underlying browser process
/// and state via `Arc`. This enables exposing `browser` as a test fixture.
#[derive(Clone)]
pub struct Browser {
  state: Arc<RwLock<BrowserState>>,
  /// Product version captured once at launch from CDP
  /// `Browser.getVersion().product`. Cached here so `version()` stays
  /// synchronous and `Arc`-shared across cheap `Browser::clone`s.
  version: Arc<str>,
  /// Negotiated backend cached for synchronous feature checks.
  backend_kind: crate::backend::BackendKind,
  instance: Arc<str>,
  isolated_contexts: bool,
  /// Headless flag cached at construction so `is_headless()` stays sync
  /// without needing to grab the outer `RwLock`.
  headless: bool,
  /// Direct handle to [`BrowserState::context_options`] so the sync
  /// `new_context` setter can register the options bag without having
  /// to obtain the outer `RwLock` read guard. Cloned at launch from
  /// the state and again in [`Self::from_shared_state`].
  context_options: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
  /// Mirror of [`BrowserState::record_video`] for the same reason — so
  /// a caller that only sets `record_video` via the bag still gets the
  /// per-page recording runtime kicked off in
  /// [`crate::context::ContextRef::new_page`]. Kept alongside
  /// `context_options` until the video-only registry is retired.
  record_video: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::RecordVideoOptions>>>,
  /// Ordered registry of live context names (`"default"` plus every
  /// `new_context`), so `contexts()` stays sync like Playwright's
  /// `browser.contexts()` — no `RwLock<BrowserState>` read, no
  /// per-page round-trip.
  context_names: Arc<std::sync::Mutex<Vec<ContextEntry>>>,
  /// Shared handle to [`BrowserState::connected`] so `is_connected()`
  /// stays sync like Playwright's `browser.isConnected(): boolean`.
  connected: Arc<std::sync::atomic::AtomicBool>,
  /// Browser-level event emitter (`'context'`, `'disconnected'`).
  /// Cheap to clone (Arc-shared) so every `Browser::clone` observes the
  /// same listeners.
  events: crate::events::BrowserEventEmitter,
  /// Set once the connection ended; `disconnected` fires on that edge.
  disconnected: Arc<std::sync::atomic::AtomicBool>,
}

/// One entry in [`Browser::context_names`].
///
/// `listed` is false only between [`Browser::new_context_unlisted`] and
/// [`Browser::publish_context`]. A context in that window exists in the
/// browser but has no owner yet, so reporting it from `contexts()` — or
/// announcing it via `browser.on('context')` — would show a caller a
/// container it was never handed.
struct ContextEntry {
  name: String,
  listed: bool,
}

fn default_context_registry() -> Arc<std::sync::Mutex<Vec<ContextEntry>>> {
  Arc::new(std::sync::Mutex::new(vec![ContextEntry {
    name: "default".to_string(),
    listed: true,
  }]))
}

impl Browser {
  /// Construct from already-prepared component handles. Used by
  /// [`crate::browser_type`] after `state.ensure_browser()` has run
  /// and by callers who need to supply pre-resolved version/registry
  /// handles (the test runner). The expected single-source-of-truth
  /// path to construct a `Browser` is the `BrowserType` factory.
  pub(crate) fn from_parts(
    state: Arc<RwLock<BrowserState>>,
    version: Arc<str>,
    backend_kind: crate::backend::BackendKind,
    headless: bool,
    context_options: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
    record_video: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::RecordVideoOptions>>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
  ) -> Self {
    let isolated_contexts = state.try_read().ok().is_none_or(|state| {
      state
        .default_browser()
        .is_none_or(crate::backend::AnyBrowser::supports_isolated_contexts)
    });
    Self {
      isolated_contexts,
      state,
      version,
      backend_kind,
      instance: Arc::from("default"),
      headless,
      context_options,
      record_video,
      connected,
      context_names: default_context_registry(),
      events: crate::events::BrowserEventEmitter::new(),
      disconnected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
  }

  /// Infra constructor: wrap a [`BrowserState`] whose
  /// `ensure_browser()` has already completed. `BrowserType::launch`
  /// is the user-facing path; this entry point exists for
  /// ferridriver-internal callers (the test runner / test fixtures /
  /// MCP server) that build a [`crate::options::LaunchPlan`] directly
  /// and need a matching `Browser` handle.
  ///
  /// # Safety contract
  ///
  /// The caller MUST have awaited `state.ensure_browser()` (or an
  /// equivalent `ensure_instance(...)` call) before handing the state
  /// in — otherwise `version()` will return `"Unknown"` until a
  /// subsequent ensure.
  #[must_use]
  pub fn from_state(state: BrowserState) -> Self {
    let closed = state.default_browser().map(crate::backend::AnyBrowser::closed);
    Self::from_state_unwatched(state).watch_connection(closed)
  }

  fn from_state_unwatched(state: BrowserState) -> Self {
    let version: Arc<str> = state
      .default_browser()
      .map(crate::backend::AnyBrowser::version)
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    let backend_kind = state
      .default_browser()
      .map_or(state.backend_kind(), crate::backend::AnyBrowser::kind);
    let headless = state.headless;
    let context_options = state.context_options.clone();
    let record_video = state.record_video.clone();
    let connected = state.connected.clone();
    Self::from_parts(
      Arc::new(RwLock::new(state)),
      version,
      backend_kind,
      headless,
      context_options,
      record_video,
      connected,
    )
  }

  /// Keep context creation and protocol sessions on the selected instance.
  ///
  /// # Errors
  /// Returns an error if the instance has not completed its connection.
  pub fn from_instance_state(state: BrowserState, instance: &str) -> Result<Self> {
    let backend = state
      .instance_browser(instance)
      .ok_or_else(|| crate::FerriError::invalid_argument("instance", "browser has not been connected"))?;
    let version = backend.version();
    let isolated_contexts = backend.supports_isolated_contexts();
    let backend_kind = backend.kind();
    let closed = backend.closed();
    let default_context = format!("{instance}:default");
    let default_is_live = state.context(&default_context).is_ok();
    let mut browser = Self::from_state_unwatched(state);
    browser.version = Arc::from(version);
    browser.backend_kind = backend_kind;
    browser.isolated_contexts = isolated_contexts;
    browser.instance = Arc::from(instance);
    browser.context_names = Arc::new(std::sync::Mutex::new(vec![ContextEntry {
      name: default_context,
      listed: default_is_live,
    }]));
    Ok(browser.watch_connection(Some(closed)))
  }

  pub(crate) async fn from_ready_state(state: Arc<RwLock<BrowserState>>, instance: &str) -> Result<Self> {
    let guard = state.read().await;
    guard.ensure_not_disposed()?;
    let backend = guard
      .instance_browser(instance)
      .ok_or_else(|| crate::FerriError::target_closed(None))?;
    let closed = backend.closed();
    let browser = Self {
      state: Arc::clone(&state),
      version: Arc::from(backend.version()),
      backend_kind: backend.kind(),
      instance: Arc::from(instance),
      isolated_contexts: backend.supports_isolated_contexts(),
      headless: guard.headless,
      context_options: Arc::clone(&guard.context_options),
      record_video: Arc::clone(&guard.record_video),
      context_names: default_context_registry(),
      connected: Arc::clone(&guard.connected),
      events: crate::events::BrowserEventEmitter::new(),
      disconnected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    drop(guard);
    Ok(browser.watch_connection(Some(closed)))
  }

  /// Follow the browser connection: when it ends, by `close()` or
  /// because the browser went away, report it once as `disconnected`.
  fn watch_connection(self, closed: Option<futures::future::BoxFuture<'static, ()>>) -> Self {
    if let (Some(closed), Ok(runtime)) = (closed, tokio::runtime::Handle::try_current()) {
      let browser = self.clone();
      runtime.spawn(async move {
        closed.await;
        browser.did_disconnect().await;
      });
    }
    self
  }

  /// The browser went away on its own: its contexts close, then
  /// `disconnected` fires, once (Playwright's `Browser._didClose`).
  async fn did_disconnect(&self) {
    if self.disconnected.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return;
    }
    let contexts = self.live_contexts().await;
    self.announce_disconnect(contexts);
  }

  /// The contexts as their own emitters, which removing a context from
  /// the state forgets: resolve them before the browser shuts down.
  /// Awaited rather than the sync constructor's fallback, which hands
  /// back an emitter nobody listens on while the state is locked.
  async fn live_contexts(&self) -> Vec<ContextRef> {
    let names: Vec<String> = self
      .context_names
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .filter(|entry| entry.listed)
      .map(|entry| entry.name.clone())
      .collect();
    let mut contexts = Vec::with_capacity(names.len());
    for name in names {
      contexts.push(
        ContextRef::new_async(self.state.clone(), name)
          .await
          .with_browser(self.clone()),
      );
    }
    contexts
  }

  fn announce_disconnect(&self, contexts: Vec<ContextRef>) {
    for context in contexts {
      context.did_close();
    }
    self
      .events
      .emit(crate::events::BrowserEvent::Disconnected(self.clone()));
  }

  /// Wrap an existing shared state as a Browser handle.
  /// Used by MCP server and other contexts that already manage browser state.
  ///
  /// The version string is read once from the state's default instance; if
  /// the instance has not been launched yet, `version()` returns
  /// `"Unknown"` until a subsequent `ensure_browser` fills it in.
  pub fn from_shared_state(state: Arc<RwLock<BrowserState>>) -> Self {
    let (version, backend_kind, headless, context_options, record_video, connected) =
      state.try_read().ok().map_or_else(
        || {
          (
            Arc::from("Unknown"),
            crate::backend::BackendKind::CdpPipe,
            true,
            Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
            Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
          )
        },
        |s| {
          (
            s.default_browser()
              .map(crate::backend::AnyBrowser::version)
              .map_or_else(|| Arc::<str>::from("Unknown"), Arc::from),
            s.default_browser()
              .map_or(s.backend_kind(), crate::backend::AnyBrowser::kind),
            s.headless,
            s.context_options.clone(),
            s.record_video.clone(),
            s.connected.clone(),
          )
        },
      );
    let isolated_contexts = state.try_read().ok().is_none_or(|state| {
      state
        .default_browser()
        .is_none_or(crate::backend::AnyBrowser::supports_isolated_contexts)
    });
    Self {
      isolated_contexts,
      state,
      version,
      backend_kind,
      instance: Arc::from("default"),
      headless,
      context_options,
      record_video,
      connected,
      context_names: default_context_registry(),
      events: crate::events::BrowserEventEmitter::new(),
      disconnected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext(options?)` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
  ///
  /// Options are stored on the shared
  /// [`crate::state::BrowserState::context_options`] registry keyed by
  /// composite session key and consumed by
  /// [`ContextRef::new_page`] as each page is opened. The registry
  /// itself is a plain `std::sync::Mutex` clone-handle on `self` so
  /// the underlying setter stays sync regardless of whether an async
  /// writer holds the outer `RwLock<BrowserState>`.
  #[track_caller]
  pub fn new_context(&self) -> crate::action::Action<'static, crate::options::BrowserContextOptions, ContextRef> {
    let this = self.clone();
    crate::action::Action::new(move |opts: crate::options::BrowserContextOptions| {
      Box::pin(async move {
        if !this.supports_isolated_contexts() {
          return Err(crate::FerriError::unsupported(
            "This browser does not support isolated contexts; use its default context",
          ));
        }
        let restore = opts.clone();
        let context = this.new_context_impl(Some(opts), true);
        context.restore_storage_state_credentials(&restore).await?;
        Ok(context)
      })
    })
  }

  /// Create a context that [`Self::contexts`] does not report and that
  /// `browser.on('context')` does not announce until
  /// [`Self::publish_context`] hands it to an owner.
  ///
  /// Exists for pre-creation: opening a context and its first page costs
  /// a renderer-process spawn, so a caller that knows it will need one
  /// shortly can pay that cost early, off the critical path. Until the
  /// context is published it is the creator's private resource, and
  /// nothing observing the browser should see it. Publishing is
  /// idempotent; a context that is closed without ever being published
  /// is never announced at all.
  #[track_caller]
  pub fn new_context_unlisted(
    &self,
  ) -> crate::action::Action<'static, crate::options::BrowserContextOptions, ContextRef> {
    let this = self.clone();
    crate::action::Action::new(move |opts: crate::options::BrowserContextOptions| {
      Box::pin(async move {
        if !this.supports_isolated_contexts() {
          return Err(crate::FerriError::unsupported(
            "This browser does not support isolated contexts; use its default context",
          ));
        }
        let restore = opts.clone();
        let context = this.new_context_impl(Some(opts), false);
        context.restore_storage_state_credentials(&restore).await?;
        Ok(context)
      })
    })
  }

  /// Drop a context from the [`Self::contexts`] listing.
  ///
  /// Called when the context closes. Without this the registry is
  /// append-only: a process that opens and closes a context per session —
  /// which is what a test runner and a synthetic-monitoring loop both do —
  /// reports every context it has ever created from `contexts()`, forever,
  /// and holds the name strings for the life of the process.
  pub(crate) fn unregister_context(&self, name: &str) {
    let mut names = match self.context_names.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    names.retain(|entry| entry.name != name);
  }

  /// Make a context from [`Self::new_context_unlisted`] visible to
  /// [`Self::contexts`] and fire the deferred `'context'` event. No-op
  /// for a context that is already listed.
  pub fn publish_context(&self, ctx: &ContextRef) {
    {
      let mut names = match self.context_names.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
      };
      match names.iter_mut().find(|e| e.name == *ctx.name()) {
        Some(entry) if !entry.listed => entry.listed = true,
        _ => return,
      }
    }
    self.emit_context_created(ctx.clone());
  }

  pub(crate) fn new_context_impl(
    &self,
    options: Option<crate::options::BrowserContextOptions>,
    listed: bool,
  ) -> ContextRef {
    static CTX_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = if &*self.instance == "default" {
      format!("context-{id}")
    } else {
      format!("{}:context-{id}", self.instance)
    };
    {
      let mut names = match self.context_names.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
      };
      names.push(ContextEntry {
        name: name.clone(),
        listed,
      });
    }
    let ctx = ContextRef::new(self.state.clone(), name).with_browser(self.clone());
    if let Some(opts) = options {
      self.seed_context_options(&ctx, opts);
    }
    if listed {
      self.emit_context_created(ctx.clone());
    }
    ctx
  }

  /// Record the options a context starts with, where its pages read
  /// them as they open.
  fn seed_context_options(&self, ctx: &ContextRef, opts: crate::options::BrowserContextOptions) {
    let composite = ctx.key.to_composite();
    if let Some(ref attr) = opts.test_id_attribute {
      ctx.set_test_id_attribute(attr.clone());
    }
    // Mirror `record_video` into the legacy per-video registry too,
    // so the recording runtime (which still reads via
    // `BrowserState::get_record_video`) continues to kick in on
    // every new_page without waiting for that registry to be
    // retired.
    if let Some(ref rv) = opts.record_video {
      let mut rv_map = match self.record_video.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
      };
      rv_map.insert(composite.clone(), rv.clone());
    }
    let mut map = match self.context_options.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    map.insert(composite, opts);
  }

  /// Hand the default context to its next user as if it had just been
  /// created, with `options` and one blank page.
  ///
  /// Playwright's `server/browser.ts::newContextForReuse` and the
  /// `resetForReuse` behind it, which its runner calls where tests share
  /// a context. Android Chrome is that case here: the device's context is
  /// the only one it has, so nothing else separates one test from the
  /// next.
  ///
  /// What the previous user left goes: its pages, its routes, bindings,
  /// init scripts, listeners and recordings, the cookies, the granted
  /// permissions, and the storage of every origin it visited. A
  /// [`ContextRef`] obtained before the reset keeps the listeners it was
  /// given and hears nothing further.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser is gone, or if clearing the old
  /// state or opening the page fails. The context is left with a page
  /// either way: a browser whose one context is the device's must never
  /// be without a tab.
  pub async fn reset_default_context(
    &self,
    options: crate::options::BrowserContextOptions,
  ) -> crate::error::Result<(ContextRef, Arc<crate::Page>)> {
    let previous = self.default_context();
    Box::pin(crate::state::BrowserState::ensure_instance_shared(
      &self.state,
      &previous.key.instance,
    ))
    .await?;
    previous.flush_har_updates().await?;
    previous.release_registrations().await?;
    let (visited, old_pages) = {
      let mut state = self.state.write().await;
      let visited = state.visited_origins.of(&previous.key.to_composite());
      (visited, state.reset_context(previous.name()).await)
    };

    // The registries are gone, so this handle gets the context's new
    // emitter, and the pages it opens carry nothing over.
    let context = self.default_context();
    let keeper = context.open_storage_page().await?;
    let cleared = async {
      for page in old_pages {
        if let Err(error) = page.close_page(crate::options::PageCloseOptions::default()).await {
          tracing::debug!(target: "ferridriver::browser", %error, "reset: closing a page of the previous user failed");
        }
        page.dispose_local();
      }
      keeper.inner().clear_cookies().await?;
      match keeper.inner().reset_permissions().await {
        // A backend with no permission overrides granted none to take back.
        Err(error) if error.is_unsupported() => {},
        other => other?,
      }
      // A Classic WebDriver session reads and deletes the cookies of the
      // document it is on and no others, so the one above took nothing
      // from a blank page: each origin's go while the keeper stands on it.
      let cookies_follow_the_document = keeper.inner().kind() == crate::backend::BackendKind::WebDriver;
      for origin in &visited {
        keeper.restore_origin_storage(origin, None).await?;
        if cookies_follow_the_document {
          keeper.inner().clear_cookies().await?;
        }
      }
      Ok::<(), crate::FerriError>(())
    }
    .await;

    let restore = options.clone();
    self.seed_context_options(&context, options);
    let page = Box::pin(context.new_page()).await;
    let _ = keeper.close_impl(None).await;
    cleared?;
    let page = page?;
    context.restore_storage_state_credentials(&restore).await?;
    Ok((context, page))
  }

  /// Fire `browser.on('context')` for an explicitly-created context (not
  /// the default one). `browserContext.ts` emits from
  /// `_browser._didCreateContext`; `new_context` is the equivalent choke
  /// point — `default_context()` deliberately does not emit.
  ///
  /// Emit on the next runtime tick rather than inline: `new_context` is
  /// synchronous, so a caller using the Playwright idiom
  /// `Promise.all([browser.waitForEvent('context'), browser.newContext()])`
  /// hasn't armed its (lazily-polled) waitForEvent subscription yet when
  /// this returns. Deferring lets that subscription land first, matching
  /// Playwright where newContext is async and the event fires after an
  /// await point. Listeners registered synchronously via `on`/`once`
  /// (which subscribe eagerly) still receive it.
  fn emit_context_created(&self, ctx: ContextRef) {
    let event = crate::events::BrowserEvent::Context(ctx);
    match tokio::runtime::Handle::try_current() {
      Ok(handle) => {
        let emitter = self.events.clone();
        handle.spawn(async move { emitter.emit(event) });
      },
      Err(_) => self.events.emit(event),
    }
  }

  /// Browser-level event emitter (`browser.on('context', ...)`). Cheap
  /// to clone; shared across every `Browser::clone`.
  #[must_use]
  pub fn events(&self) -> &crate::events::BrowserEventEmitter {
    &self.events
  }

  /// Register a browser-level event listener (`'context'`). Playwright:
  /// `browser.on(event, listener)`. Returns a [`crate::events::ListenerId`].
  pub fn on(&self, event_name: &str, callback: crate::events::BrowserEventCallback) -> crate::events::ListenerId {
    self.events.on(event_name, callback)
  }

  /// Single-shot variant of [`Self::on`].
  pub fn once(&self, event_name: &str, callback: crate::events::BrowserEventCallback) -> crate::events::ListenerId {
    self.events.once(event_name, callback)
  }

  /// Remove a browser-level listener by id.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.events.off(id);
  }

  /// Node's `prependListener`: run before every listener already
  /// attached to `event_name`.
  pub fn prepend_listener(
    &self,
    event_name: &str,
    callback: crate::events::BrowserEventCallback,
  ) -> crate::events::ListenerId {
    self.events.prepend(event_name, callback)
  }

  /// Node's `prependOnceListener`.
  pub fn prepend_once_listener(
    &self,
    event_name: &str,
    callback: crate::events::BrowserEventCallback,
  ) -> crate::events::ListenerId {
    self.events.prepend_once(event_name, callback)
  }

  /// Remove every browser-level listener.
  pub fn remove_all_listeners(&self) {
    self.events.remove_all_listeners();
  }

  /// Remove every browser-level listener for `event_name`.
  pub fn remove_listeners_named(&self, event_name: &str) {
    self.events.remove_listeners_named(event_name);
  }

  /// Node's `listenerCount(type)`.
  #[must_use]
  pub fn listener_count(&self, event_name: &str) -> usize {
    self.events.listener_count(event_name)
  }

  /// Node's `eventNames()`.
  #[must_use]
  pub fn event_names(&self) -> Vec<String> {
    self.events.event_names()
  }

  /// The listener ids registered for `event_name`, in dispatch order.
  #[must_use]
  pub fn listener_ids_named(&self, event_name: &str) -> Vec<crate::events::ListenerId> {
    self.events.listener_ids_named(event_name)
  }

  /// Node's `setMaxListeners(n)`; `0` disables the leak warning.
  pub fn set_max_listeners(&self, max: usize) {
    self.events.set_max_listeners(max);
  }

  /// Node's `getMaxListeners()`.
  #[must_use]
  pub fn max_listeners(&self) -> usize {
    self.events.max_listeners()
  }

  /// Wait for the next browser-level event matching `event_name`.
  /// Playwright: `browser.waitForEvent(event, options?)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the browser closes.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> Result<crate::events::BrowserEvent> {
    self.events.wait_for_event(event_name, timeout_ms).await
  }

  /// Get the default browser context.
  #[must_use]
  pub fn default_context(&self) -> ContextRef {
    let name = if &*self.instance == "default" {
      "default".to_owned()
    } else {
      format!("{}:default", self.instance)
    };
    ContextRef::new(self.state.clone(), name).with_browser(self.clone())
  }

  /// Whether the connected browser supports isolated contexts.
  /// Android Chrome exposes only its device context through CDP.
  #[must_use]
  pub fn supports_isolated_contexts(&self) -> bool {
    self.isolated_contexts
  }

  /// Backend kind negotiated by the connected instance.
  #[must_use]
  pub fn backend_kind(&self) -> crate::backend::BackendKind {
    self.backend_kind
  }

  /// Whether the browser was launched in headless mode. Cached at
  /// construction; the launch plan never flips this after the fact.
  #[must_use]
  pub fn is_headless(&self) -> bool {
    self.headless
  }

  /// Shorthand: create a new page in the default context.
  /// Equivalent to `browser.default_context().new_page()`.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Arc<Page>> {
    Box::pin(self.default_context().new_page()).await
  }

  /// Shorthand: create a new page and navigate to URL.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or navigation fails.
  pub async fn new_page_with_url(&self, url: &str) -> Result<Arc<Page>> {
    let page = Box::pin(self.new_page()).await?;
    if let Err(error) = page.goto_impl(url, None).await {
      if let Err(cleanup) = page.close().await {
        return Err(crate::error::FerriError::backend(format!(
          "Opening page failed: {error}; closing its window failed: {cleanup}"
        )));
      }
      return Err(error);
    }
    Ok(page)
  }

  /// Shorthand: get the active page in the default context.
  /// Creates a page if none exists.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or retrieval fails.
  ///
  pub async fn page(&self) -> Result<Arc<Page>> {
    let ctx = self.default_context();
    let mut pages = ctx.discover_pages().await.unwrap_or_default();
    if pages.is_empty() {
      Box::pin(ctx.new_page()).await
    } else {
      Ok(pages.swap_remove(0))
    }
  }

  /// Close the browser.
  ///
  /// Mirrors Playwright's `browser.close({ reason })`; the option bag is
  /// [`crate::options::BrowserCloseOptions`]. The reason, if set, is
  /// surfaced on `TargetClosed` errors emitted to any in-flight
  /// operation on pages/contexts from this browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser cannot be closed cleanly.
  #[track_caller]
  pub fn close(&self) -> crate::action::Action<'static, crate::options::BrowserCloseOptions, ()> {
    let this = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { this.close_impl(Some(opts)).await }))
  }

  pub(crate) async fn close_impl(&self, opts: Option<crate::options::BrowserCloseOptions>) -> Result<()> {
    let contexts = self.live_contexts().await;
    let first_close = !self.disconnected.swap(true, std::sync::atomic::Ordering::SeqCst);
    let mut state = self.state.write().await;
    if let Some(reason) = opts.and_then(|o| o.reason) {
      state.set_close_reason(reason);
    }
    let result = state.shutdown().await;
    drop(state);
    if first_close {
      self.announce_disconnect(contexts);
    }
    result
  }

  /// Access the internal state (for MCP server integration).
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    &self.state
  }

  /// List all browser contexts. Sync — mirrors Playwright's
  /// `browser.contexts(): BrowserContext[]`. Reads the in-memory name
  /// registry (default + every `new_context`); no state lock, no
  /// per-page round-trip.
  #[must_use]
  pub fn contexts(&self) -> Vec<ContextRef> {
    let names = match self.context_names.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    names
      .iter()
      .filter(|entry| entry.listed)
      .map(|entry| ContextRef::new(self.state.clone(), entry.name.clone()).with_browser(self.clone()))
      .collect()
  }

  pub(crate) fn owns_context(&self, name: &str) -> bool {
    self
      .context_names
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .any(|entry| entry.name == name)
  }

  /// Real product version string for the running browser — mirrors
  /// Playwright's synchronous `browser.version()`.
  ///
  /// Captured once at handshake. For CDP it's `Browser.getVersion().product`
  /// (e.g. `"HeadlessChrome/120.0.6099.109"` or `"Chrome/120.0.6099.109"`);
  /// for Playwright `WebKit` it's `"webkit-playwright/{revision}"` from the
  /// launcher; for `BiDi` it's `"{browserName}/{browserVersion}"` from the
  /// session capabilities. Returns `"Unknown"` if the handshake did not
  /// complete before the `Browser` handle was constructed.
  #[must_use]
  pub fn version(&self) -> &str {
    &self.version
  }

  /// Whether the browser is connected. Sync — mirrors Playwright's
  /// `browser.isConnected(): boolean`.
  #[must_use]
  pub fn is_connected(&self) -> bool {
    self.connected.load(std::sync::atomic::Ordering::Relaxed)
      && !self.disconnected.load(std::sync::atomic::Ordering::SeqCst)
  }

  /// Attach a raw CDP session to the browser target. Playwright:
  /// `browser.newBrowserCDPSession()` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browser.ts:157`.
  /// Chromium-only.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] on WebKit/BiDi,
  /// an error if the browser is not launched, or the protocol error if
  /// the attach fails.
  pub async fn new_browser_cdp_session(&self) -> crate::error::Result<crate::cdp_session::CdpSession> {
    let state = self.state.read().await;
    let browser = state
      .instance_browser(&self.instance)
      .ok_or_else(|| crate::error::FerriError::backend("browser is not launched".to_string()))?;
    browser.new_browser_cdp_session().await
  }
}
