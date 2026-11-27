//! Context cookie jar, `storageState` persistence, and `HttpClient` bridge.

use std::sync::Arc;

use super::ContextRef;
use crate::backend::{AnyPage, CookieData};
use crate::error::Result;
use crate::page::Page;

/// Where [`ContextRef`] reads and writes cookies: the context's live
/// page, or a page of it opened for the purpose and closed after.
struct CookieJar {
  page: AnyPage,
  opened: Option<Arc<Page>>,
}

impl CookieJar {
  fn page(&self) -> &AnyPage {
    &self.page
  }

  async fn done(self) -> Result<()> {
    match self.opened {
      Some(page) => page.close_impl(None).await,
      None => Ok(()),
    }
  }
}

impl ContextRef {
  /// Playwright: `browserContext.cookies(urls?)`. The cookies a request
  /// to any of `urls` would carry; all of them without one.
  ///
  /// # Errors
  ///
  /// Returns an error for a URL that does not parse, or if the context
  /// does not exist or its cookies cannot be read.
  pub async fn cookies_for(&self, urls: &[String]) -> Result<Vec<CookieData>> {
    crate::http_client::cookies_for_urls(self.cookies().await?, urls)
  }

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or cookie retrieval fails.
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    self
      .traced("cookies", serde_json::json!({}), async {
        let Some(jar) = self.cookie_jar(false).await? else {
          return Ok(Vec::new());
        };
        let cookies = jar.page().get_cookies().await;
        let done = jar.done().await;
        cookies.and_then(|cookies| done.map(|()| cookies))
      })
      .await
  }

  /// Export the current storage state of this context: cookies plus a
  /// per-origin snapshot of `localStorage` and, when asked for,
  /// `IndexedDB` and the origin private file system.
  ///
  /// Playwright: `storageState(options?: { path?: string, indexedDB?:
  ///   boolean, opfs?: boolean, credentials?: boolean }): Promise<StorageState>`
  /// (`packages/playwright-core/src/client/browserContext.ts`; server
  /// collection in `server/browserContext.ts`).
  ///
  /// Cookies are read via the existing [`Self::cookies`] surface. Each
  /// live page runs Playwright's `StorageScript.collect` for its origin;
  /// origins with nothing stored are left out. An origin the context
  /// visited that no page is on any more is not collected: upstream
  /// revisits those on a blank page, which this does not do. `opts.path`,
  /// when set, writes the JSON-serialized state (pretty-printed) to disk.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist, cookie retrieval fails,
  /// or (when `path` is set) the file cannot be written.
  #[track_caller]
  pub fn storage_state(
    &self,
  ) -> crate::action::Action<'static, crate::options::StorageStateOptions, crate::options::StorageState> {
    let this = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { Box::pin(this.storage_state_impl(Some(opts))).await }))
  }

  pub(crate) async fn storage_state_impl(
    &self,
    opts: Option<crate::options::StorageStateOptions>,
  ) -> Result<crate::options::StorageState> {
    let indexed_db = opts.as_ref().and_then(|o| o.indexed_db).unwrap_or(false);
    let file_system = opts.as_ref().and_then(|o| o.opfs).unwrap_or(false);

    // A context opens lazily, with its first page: one that never had a
    // page has no cookies to read yet, only what it was created with.
    let cookies = if self.is_open().await {
      self.cookies().await?
    } else {
      Vec::new()
    };

    let pages = self.discover_pages().await?;
    let mut origins: Vec<crate::options::OriginState> = Vec::new();
    // Origins whose storage has been read, with or without anything in it.
    let mut read: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    let collect = |page: &Page| {
      crate::selectors::storage_script_call(
        page.inner.kind(),
        &format!(
          "script.collect({{ indexedDB: {indexed_db}, opfs: {file_system} }}).then(storage => ({{ origin: location.origin, ...storage }}))"
        ),
      )
    };
    let parse = |raw: &serde_json::Value| {
      raw
        .as_str()
        .and_then(|s| serde_json::from_str::<crate::options::OriginState>(s).ok())
        .filter(|state| !state.origin.is_empty() && state.origin != "null")
    };

    for page in &pages {
      let raw = match page.inner.evaluate(&collect(page)).await {
        Ok(Some(raw)) => raw,
        // What was asked for and could not be read is an error, as it is
        // upstream once the origin is retried on a blank page. Anything
        // else is a page with no storage to speak of: an opaque origin,
        // or one caught mid-navigation.
        Err(error) if error.to_string().contains("Unable to serialize") => return Err(error),
        Ok(None) | Err(_) => continue,
      };
      let Some(state) = parse(&raw) else { continue };
      if read.insert(state.origin.clone()) && state.has_storage() {
        origins.push(state);
      }
    }

    // Playwright's `visitOrigins`: what the context visited and no live
    // page is on any more (a closed page, a page that navigated away, a
    // cross-origin frame) is read from a page of its own.
    let unread: Vec<String> = self
      .state
      .read()
      .await
      .visited_origins
      .of(&self.key.to_composite())
      .into_iter()
      .filter(|origin| !read.contains(origin))
      .collect();
    if !unread.is_empty() && self.is_open().await {
      let page = self.open_storage_page().await?;
      let visited = async {
        for origin in &unread {
          page.goto_blank_origin(origin).await?;
          let Some(raw) = page.inner.evaluate(&collect(&page)).await? else {
            continue;
          };
          if let Some(state) = parse(&raw).filter(crate::options::OriginState::has_storage) {
            origins.push(state);
          }
        }
        Ok::<(), crate::error::FerriError>(())
      }
      .await;
      let _ = page.close_impl(None).await;
      visited?;
    }

    let credentials = match opts.as_ref().and_then(|o| o.credentials) {
      Some(true) => Some(
        self
          .credentials()
          .get(crate::credentials::CredentialFilter::default())
          .await,
      ),
      _ => None,
    };
    let state = crate::options::StorageState {
      cookies,
      origins,
      credentials,
    };

    if let Some(opts) = opts
      && let Some(path) = opts.path
    {
      let json = serde_json::to_string_pretty(&state)
        .map_err(|e| crate::error::FerriError::Backend(format!("storageState: serialize JSON: {e}")))?;
      if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
      {
        tokio::fs::create_dir_all(parent)
          .await
          .map_err(|e| crate::error::FerriError::Backend(format!("storageState: mkdir {}: {e}", parent.display())))?;
      }
      tokio::fs::write(&path, json)
        .await
        .map_err(|e| crate::error::FerriError::Backend(format!("storageState: write {}: {e}", path.display())))?;
    }

    Ok(state)
  }

  /// Replace this context's storage state with `state`, clearing existing
  /// cookies and localStorage first. Mirrors Playwright 1.59's
  /// `browserContext.setStorageState(state)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts`.
  ///
  /// `state` accepts a JSON value in Playwright's storage-state shape
  /// (`{ cookies, origins }`). The cookies are cleared and re-seeded; each
  /// origin's localStorage is cleared on any live page for that origin before
  /// the new items are applied via [`Page::set_storage_state`].
  ///
  /// # Errors
  ///
  /// Returns an error if the context has no page to act through, or if
  /// clearing / applying cookies or localStorage fails.
  pub async fn set_storage_state(&self, state: &serde_json::Value) -> Result<()> {
    // Clear current cookies for the whole context.
    self.clear_cookies().await?;

    // Clear localStorage on every live page (each in its own origin scope).
    let pages = self.discover_pages().await?;
    for page in &pages {
      let _ = page.inner.evaluate("localStorage.clear()").await;
    }

    // A context opens in the browser with its first page.
    if pages.is_empty() && !self.is_open().await {
      Box::pin(self.new_page()).await?;
    }
    Box::pin(self.restore_storage_state(state, true)).await?;
    self.credentials().restore(state_credentials(state)?).await
  }

  /// Write `state` into the context through a page of its own, as
  /// Playwright's `setStorageState` does: cookies, then each origin's
  /// storage from a document of that origin. With `reset`, every origin
  /// the context visited is emptied too, so what `state` holds is all
  /// that remains.
  pub(super) async fn restore_storage_state(&self, state: &serde_json::Value, reset: bool) -> Result<()> {
    let composite = self.key.to_composite();
    let entries: Vec<(&str, &serde_json::Value)> = state
      .get("origins")
      .and_then(|v| v.as_array())
      .into_iter()
      .flatten()
      .filter_map(|entry| {
        let origin = entry.get("origin").and_then(|v| v.as_str())?;
        (!origin.is_empty()).then_some((origin, entry))
      })
      .collect();
    let emptied: Vec<String> = if reset {
      let visited = self.state.read().await.visited_origins.of(&composite);
      visited
        .into_iter()
        .filter(|origin| !entries.iter().any(|(restored, _)| restored == origin))
        .collect()
    } else {
      Vec::new()
    };

    let page = self.open_storage_page().await?;
    let restored = async {
      page.restore_cookies(state).await?;
      for origin in &emptied {
        page.restore_origin_storage(origin, None).await?;
      }
      for (origin, entry) in &entries {
        page.restore_origin_storage(origin, Some(entry)).await?;
      }
      Ok::<(), crate::error::FerriError>(())
    }
    .await;
    let _ = page.close_impl(None).await;
    restored?;

    self
      .state
      .read()
      .await
      .visited_origins
      .replace(&composite, entries.iter().map(|(origin, _)| (*origin).to_string()));
    Ok(())
  }

  /// A page of this context that nothing else is told about: it is in
  /// no page list, raises no `page` event, and carries none of the
  /// context's routes, scripts or recordings. Playwright's
  /// `newPage(forStorageState)`, for reading and writing storage from a
  /// document of each origin without borrowing a page the caller holds.
  pub(crate) async fn open_storage_page(&self) -> Result<Arc<Page>> {
    let plan = self.state.read().await.page_open_plan(&self.key)?;
    let page = Box::pin(
      plan
        .browser
        .new_page("about:blank", plan.browser_context_id.as_deref(), None),
    )
    .await?;
    Ok(Page::new(page))
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or setting cookies fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let count = cookies.len();
    self
      .traced("addCookies", serde_json::json!({ "cookies": count }), async move {
        // Playwright resolves a cookie's `url` here, not in the browser.
        let cookies = cookies
          .into_iter()
          .map(crate::http_client::rewrite_cookie)
          .collect::<Result<Vec<_>>>()?;
        // Before the first page Playwright still sets them, on the browser
        // context itself, so a spec may seed cookies first.
        let Some(jar) = self.cookie_jar(true).await? else {
          return Ok(());
        };
        let mut outcome = Ok(());
        for cookie in cookies {
          outcome = jar.page().set_cookie(cookie).await;
          if outcome.is_err() {
            break;
          }
        }
        outcome.and(jar.done().await)
      })
      .await
  }

  /// The context-bound HTTP client backing Playwright's
  /// `context.request` / `page.request`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:76`,
  /// server: `server/fetch.ts::BrowserContextAPIRequestContext`). Shares
  /// this context's cookie jar in both directions: outgoing requests
  /// carry the context's matching cookies, and every `Set-Cookie` in a
  /// response (including redirect hops) lands back in the browser.
  /// `baseURL`, `extraHTTPHeaders`, `userAgent`, and
  /// `ignoreHTTPSErrors` context options are read live per request.
  ///
  /// Each call mints a fresh handle; all state that matters (cookies,
  /// options) lives in the browser/context, so every handle behaves
  /// identically — the Playwright `page.request === context.request`
  /// identity holds semantically, not by object identity.
  #[must_use]
  pub fn http_client(&self) -> crate::http_client::HttpClient {
    let context = self.clone();
    crate::http_client::HttpClient::context_bound(std::sync::Arc::new(self.clone()))
      .with_context(self.clone())
      .with_live_http_credentials(std::sync::Arc::new(move || {
        let context = context.clone();
        Box::pin(async move {
          let state = context.state.read().await;
          state
            .get_context_options(&context.key.to_composite())
            .and_then(|options| options.http_credentials)
            .unwrap_or_default()
        })
      }))
      .with_live_proxy(std::sync::Arc::new({
        let context = self.clone();
        move || {
          let context = context.clone();
          Box::pin(async move { context.state.read().await.effective_proxy(&context.key.to_composite()) })
        }
      }))
  }

  /// Playwright: `context.clearCookies(options?: { name?, domain?, path?
  /// })`. Without a filter every cookie goes; with one, those matching
  /// all of it. Filters chain as setters, each a string or regex:
  ///
  /// ```ignore
  /// context.clear_cookies().await?;
  /// context.clear_cookies().name("session").domain("example.com").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist, a filter's regex
  /// does not compile, or clearing cookies fails.
  #[track_caller]
  pub fn clear_cookies(&self) -> crate::action::Action<'_, crate::backend::ClearCookieOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.clear_cookies_impl(&opts).await }))
  }

  pub(crate) async fn clear_cookies_impl(&self, options: &crate::backend::ClearCookieOptions) -> Result<()> {
    if options.is_empty() {
      self.clear_all_cookies().await
    } else {
      self.clear_cookies_matching(options).await
    }
  }

  async fn clear_all_cookies(&self) -> Result<()> {
    self
      .traced("clearCookies", serde_json::json!({}), async {
        let Some(jar) = self.cookie_jar(false).await? else {
          return Ok(());
        };
        let cleared = jar.page().clear_cookies().await;
        cleared.and(jar.done().await)
      })
      .await
  }

  async fn clear_cookies_matching(&self, options: &crate::backend::ClearCookieOptions) -> Result<()> {
    let filter = options.compile()?;
    let Some(jar) = self.cookie_jar(false).await? else {
      return Ok(());
    };
    let kept = async {
      let page = jar.page();
      let cookies = page.get_cookies().await?;
      page.clear_cookies().await?;
      for c in cookies {
        if !filter.matches(&c) {
          page.set_cookie(c).await?;
        }
      }
      Ok(())
    }
    .await;
    kept.and(jar.done().await)
  }

  /// A page to read and write this context's cookies through: its live
  /// page, or, before it has one, a page of it nothing else sees, when
  /// the context exists in the browser or `create` asks that it does.
  /// Playwright keeps cookies on the browser context, so they are there
  /// before any page.
  async fn cookie_jar(&self, create: bool) -> Result<Option<CookieJar>> {
    let live = {
      let state = self.state.read().await;
      state
        .context(&self.name)
        .ok()
        .and_then(|ctx| ctx.active_page().cloned())
    };
    if let Some(page) = live {
      return Ok(Some(CookieJar { page, opened: None }));
    }
    if create {
      Box::pin(self.ensure_backend_context()).await?;
    } else {
      let made = self
        .state
        .read()
        .await
        .page_open_plan(&self.key)
        .is_ok_and(|plan| plan.browser_context_id.is_some());
      if !made {
        return Ok(None);
      }
    }
    let opened = self.open_storage_page().await?;
    Ok(Some(CookieJar {
      page: opened.inner().clone(),
      opened: Some(opened),
    }))
  }

  /// Delete a specific cookie by name and optional domain
  /// (matches Playwright's `context.clearCookies({ name })`).
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or deleting fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let state = self.state.read().await;
    let ctx = state.context(&self.name)?;
    ctx.delete_cookie(name, domain).await
  }
}

/// The storage state a context was given, read from its file if a path.
pub(super) fn storage_state_value(storage: &crate::options::StorageStateInput) -> Result<serde_json::Value> {
  match storage {
    crate::options::StorageStateInput::Inline(v) => Ok(v.clone()),
    crate::options::StorageStateInput::Path(path) => {
      let text = std::fs::read_to_string(path)
        .map_err(|e| crate::error::FerriError::Backend(format!("storageState: read {}: {e}", path.display())))?;
      serde_json::from_str(&text).map_err(|e| {
        crate::error::FerriError::Backend(format!("storageState: parse JSON from {}: {e}", path.display()))
      })
    },
  }
}

/// The `WebAuthn` credentials of a storage state, none when it has no
/// `credentials` key.
pub(super) fn state_credentials(state: &serde_json::Value) -> Result<Vec<crate::credentials::VirtualCredential>> {
  match state.get("credentials") {
    None | Some(serde_json::Value::Null) => Ok(Vec::new()),
    Some(list) => serde_json::from_value(list.clone())
      .map_err(|e| crate::error::FerriError::invalid_argument("storageState", format!("credentials: {e}"))),
  }
}

/// The browser side of the context-bound HTTP client
/// ([`ContextRef::http_client`]): cookie reads/writes go through the
/// context's cookie surface, defaults come live from the stored context
/// options (so `setExtraHTTPHeaders` after client creation is visible,
/// matching Playwright's per-request `_defaultOptions()` read).
impl crate::http_client::ContextBridge for ContextRef {
  fn defaults(&self) -> crate::http_client::BridgeFuture<'_, crate::http_client::ContextDefaults> {
    Box::pin(async move {
      let opts = {
        let state = self.state.read().await;
        state.get_context_options(&self.key.to_composite()).unwrap_or_default()
      };
      Ok::<_, crate::http_client::FetchError>(crate::http_client::ContextDefaults {
        base_url: opts.base_url,
        extra_http_headers: opts
          .extra_http_headers
          .map(|headers| headers.into_iter().collect())
          .unwrap_or_default(),
        user_agent: opts.user_agent,
        ignore_https_errors: opts.ignore_https_errors.unwrap_or(false),
      })
    })
  }

  fn cookies(&self) -> crate::http_client::BridgeFuture<'_, Vec<CookieData>> {
    Box::pin(async move {
      match ContextRef::cookies(self).await {
        Ok(cookies) => Ok(cookies),
        // A context materializes in BrowserState on its first page open;
        // until then (`browser.newContext()` + immediate `context.request`)
        // its cookie jar is simply empty.
        Err(crate::error::FerriError::InvalidArgument { .. }) => Ok(Vec::new()),
        Err(e) => Err(crate::http_client::FetchError::Bridge(e.to_string())),
      }
    })
  }

  fn add_cookies(&self, cookies: Vec<CookieData>) -> crate::http_client::BridgeFuture<'_, ()> {
    Box::pin(async move {
      ContextRef::add_cookies(self, cookies)
        .await
        .map_err(|e| crate::http_client::FetchError::Bridge(e.to_string()))
    })
  }
}
