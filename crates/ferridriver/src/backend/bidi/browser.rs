//! `BiDi` browser -- manages contexts, pages, and browser lifecycle.

use serde_json::json;
use std::sync::Arc;

#[cfg(test)]
#[path = "close_tests.rs"]
mod close_tests;
use tracing::debug;

use super::page::BidiPage;
use super::session::BidiSession;
use crate::backend::{AnyPage, NavLifecycle};
use crate::error::{FerriError, Result};

/// The blocked requests of each popup, by its top-level context.
type PopupRequests =
  rustc_hash::FxHashMap<String, tokio::sync::mpsc::UnboundedSender<Arc<super::transport::BidiEvent>>>;

/// What the popup listener needs to turn a top-level context the browser
/// opened into a popup the pump registers.
struct PopupAdopter {
  session: Arc<BidiSession>,
  downloads_dir: Arc<tempfile::TempDir>,
  navigations: super::frames::FirstNavigations,
  taps: crate::backend::PopupTaps,
}

impl PopupAdopter {
  fn adopt(
    &self,
    popups: &mut PopupRequests,
    context_id: &str,
    user_context: String,
    opener: Option<&str>,
    claim: Arc<crate::backend::PopupClaim>,
  ) {
    let page = BidiPage::create(
      Arc::clone(&self.session),
      context_id.to_string(),
      Some(&user_context),
      Arc::clone(&self.downloads_dir),
    );
    let (requests, forwarded) = tokio::sync::mpsc::unbounded_channel();
    let first_navigation = self
      .navigations
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(context_id)
      .cloned();
    page.adopt_popup(forwarded, first_navigation);
    popups.insert(context_id.to_string(), requests);
    // Claimed inline: BidiPage::create is pure construction (no
    // wire round-trip), and readiness is the pump's business.
    let delivered = crate::backend::push_popup(
      &self.taps,
      crate::backend::PopupInfo {
        page: crate::backend::AnyPage::Bidi(page),
        browser_context_id: if user_context == "default" {
          None
        } else {
          Some(user_context)
        },
        opener_target_id: opener.map(str::to_string),
        claim,
      },
    );
    if !delivered {
      debug!("popup {context_id} observed with no pump subscribed");
    }
  }
}

/// A top-level context the popup listener could not attribute yet: its
/// id, its user context, and its place in the popups in flight.
type ParkedContext = (String, String, Arc<crate::backend::PopupClaim>);

#[derive(Clone)]
/// Browser instance using the `WebDriver` `BiDi` protocol.
pub struct BidiBrowser {
  pub(crate) session: Arc<BidiSession>,
  child: Arc<tokio::sync::Mutex<Option<crate::backend::process::ChildGroup>>>,
  /// Popup announcement subscriptions (see
  /// [`crate::backend::PopupInfo`]) fed by the connect-time
  /// `browsingContext.contextCreated` listener.
  popup_taps: crate::backend::PopupTaps,
  /// Which top-level contexts our own `browsingContext.create` made, so
  /// the listener adopts every other one: a page another client of a
  /// shared session opened, which carries no opener.
  create_ledger: Arc<crate::backend::CreateLedger<ParkedContext>>,
  /// Popups the listener has seen open and the pump has not yet
  /// registered.
  popups: crate::backend::PopupWatch,
  /// Owned Firefox `--profile` directory for launched browsers. Removed
  /// by `close()`, or by the last handle drop if nobody closed. `None`
  /// for `connect()` — we don't own the profile of a browser someone
  /// else launched.
  profile_dir: Option<Arc<crate::backend::async_tempdir::AsyncTempDir>>,
  /// One downloads directory per browser, shared by every page. Per
  /// page it cost a mkdir on each open and leaked a directory whenever
  /// teardown was skipped.
  downloads_dir: Arc<tempfile::TempDir>,
  webdriver: Option<Arc<super::webdriver::WebDriverSession>>,
}

impl BidiBrowser {
  /// Resolves once the connection to the browser has ended.
  pub fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    self.session.transport.closed()
  }

  /// Real browser version reported in the `BiDi` session capabilities at
  /// `session.new` time. Format is `"{browserName}/{browserVersion}"` to
  /// match the CDP `Browser.getVersion().product` shape (e.g.
  /// `"firefox/135.0.1"`).
  #[must_use]
  pub fn version(&self) -> String {
    format!("{}/{}", self.session.browser_name, self.session.browser_version)
  }

  async fn wait_for_context_event(&self, method: &str, context_id: &str, timeout: std::time::Duration) -> Result<()> {
    let mut rx = self.session.transport.tap_events();
    let wait_for_event = async {
      while let Some(event) = rx.recv().await {
        if event.method != method {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(context_id) {
          return Ok(());
        }
      }
      Err(FerriError::Backend(format!(
        "BiDi event stream closed while waiting for {method}"
      )))
    };
    tokio::time::timeout(timeout, wait_for_event).await.map_err(|_| {
      FerriError::timeout(
        format!("BiDi event '{method}' on {context_id}"),
        u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
      )
    })?
  }

  async fn list_context_ids_for_user_context(&self, user_context_id: &str) -> Result<Vec<String>> {
    let result = self
      .session
      .transport
      .send(&ferridriver_bidi_proto::browsing_context::GetTree {
        max_depth: None,
        root: None,
      })
      .await?;

    Ok(
      result
        .contexts
        .into_iter()
        .filter(|ctx| ctx.user_context.as_deref() == Some(user_context_id))
        .map(|ctx| ctx.context)
        .collect(),
    )
  }

  /// Launch a browser with `BiDi` support.
  /// Auto-detects Firefox vs Chrome from the binary path.
  ///
  /// `user_data_dir` selects a persistent profile ferridriver does not
  /// own; without it a throwaway profile is created and removed on
  /// teardown.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` when `browser_path` is not a Firefox binary, and a timeout
  /// when Firefox prints no `BiDi` WebSocket URL within 15 seconds. Also returns an error when
  /// `firefox_user_prefs` are invalid, the profile or downloads directory cannot be prepared,
  /// Firefox fails to spawn or exits during startup, or `session.new` or `session.subscribe` fails.
  pub async fn launch_with_flags(
    browser_path: &str,
    flags: &[String],
    env: &rustc_hash::FxHashMap<String, String>,
    user_data_dir: Option<&std::path::Path>,
    proxy: Option<&crate::options::ProxyConfig>,
    firefox_user_prefs: Option<&rustc_hash::FxHashMap<String, serde_json::Value>>,
  ) -> Result<Self> {
    // Determine if headless from flags
    let headless = flags.iter().any(|f| f == "--headless");
    let (session, mut group, profile) = Box::pin(BidiSession::launch(
      browser_path,
      flags,
      headless,
      env,
      user_data_dir,
      proxy,
      firefox_user_prefs,
    ))
    .await?;
    let session = Arc::new(session);
    let downloads_dir = new_downloads_dir()?;
    let create_ledger = Arc::new(crate::backend::CreateLedger::default());
    let (popup_taps, popups) = Self::spawn_popup_listener(&session, &downloads_dir, &create_ledger);
    group.own_dir(downloads_dir.path());
    Ok(Self {
      session,
      child: Arc::new(tokio::sync::Mutex::new(Some(group))),
      popup_taps,
      create_ledger,
      popups,
      // A persistent profile is the caller's, so nothing here may delete
      // it — that is the whole point of asking for one.
      profile_dir: profile
        .owned
        .map(|dir| Arc::new(crate::backend::async_tempdir::AsyncTempDir::new(dir))),
      downloads_dir,
      webdriver: None,
    })
  }

  /// # Errors
  /// Reports unsupported hosts, driver startup failures, and session negotiation errors.
  pub async fn launch_safari(env: &rustc_hash::FxHashMap<String, String>, timeout_ms: Option<u64>) -> Result<Self> {
    let timeout_ms = timeout_ms.unwrap_or(30_000);
    let (mut browser, group) =
      crate::backend::webdriver::launcher::launch_safari(env, timeout_ms, |endpoint| async move {
        Box::pin(Self::connect_webdriver(
          &endpoint,
          "safari",
          None,
          None,
          Some(timeout_ms),
        ))
        .await
      })
      .await?;
    browser.child = group;
    Ok(browser)
  }

  /// Launch Ladybird through its `WebDriver` service and drive the `BiDi`
  /// session it creates.
  ///
  /// # Errors
  /// Reports driver startup failures, a session that does not negotiate
  /// `BiDi`, and a launch that exceeds `timeout_ms` (30 seconds by default).
  pub async fn launch_ladybird(
    webdriver: &std::path::Path,
    headless: bool,
    user_data_dir: Option<&std::path::Path>,
    env: &rustc_hash::FxHashMap<String, String>,
    timeout_ms: Option<u64>,
  ) -> Result<Self> {
    let timeout_ms = timeout_ms.unwrap_or(30_000);
    let (mut browser, group) = crate::backend::webdriver::launcher::launch_ladybird(
      webdriver,
      headless,
      user_data_dir,
      env,
      timeout_ms,
      |endpoint| async move {
        Box::pin(Self::connect_webdriver(
          &endpoint,
          "ladybird",
          None,
          None,
          Some(timeout_ms),
        ))
        .await
      },
    )
    .await?;
    browser.child = group;
    Ok(browser)
  }

  /// Connect to an existing `BiDi` endpoint via WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error when the WebSocket connection to `ws_url` fails, when `session.new` or
  /// `session.subscribe` fails, or when the downloads directory cannot be created.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    Self::connect_with_headers(ws_url, None).await
  }

  pub(crate) async fn connect_with_headers(
    ws_url: &str,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<Self> {
    let session = Arc::new(Box::pin(BidiSession::connect_with_options(ws_url, None, headers)).await?);
    Self::from_session(session)
  }

  /// Create a `BiDi` browser from a `WebDriver` Classic HTTP endpoint.
  ///
  /// The endpoint creates a W3C session with `webSocketUrl: true`, then the
  /// returned `BiDi` connection carries all browser operations. This is the
  /// low-latency path for `WebDriver` servers that implement `BiDi`, including
  /// compatible Appium and Safari Technology Preview sessions.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::InvalidArgument` when `extra_capabilities` is not an object or the
  /// endpoint or a header is invalid, `FerriError::Unsupported` when the created session has no
  /// `webSocketUrl` capability, and a timeout when attaching exceeds `timeout_ms`. Otherwise
  /// forwards errors from the `WebDriver` `POST /session` command, the WebSocket connection, or
  /// `session.subscribe`, deleting the Classic session first.
  pub async fn connect_webdriver(
    endpoint: &str,
    browser_name: &str,
    extra_capabilities: Option<&serde_json::Value>,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
    timeout_ms: Option<u64>,
  ) -> Result<Self> {
    let started = tokio::time::Instant::now();
    let timeout_ms = timeout_ms.unwrap_or(30_000);
    if extra_capabilities.is_some_and(|value| !value.is_object()) {
      return Err(FerriError::invalid_argument("capabilities", "expected an object"));
    }
    let session_url = webdriver_session_url(endpoint)?;
    let always_match = crate::backend::webdriver::session::CapabilityRequest::preferring_insecure_certs(
      webdriver_capabilities(browser_name, extra_capabilities),
    );
    let client = super::webdriver::http_client(headers)?;
    let created = crate::backend::webdriver::session::WebDriverSession::create(
      client,
      session_url,
      always_match,
      timeout_ms,
      headers,
    )
    .await?;
    let owner = Arc::clone(&created.session);
    let attach = Box::pin(Self::from_created(created, endpoint, headers));
    let result = if timeout_ms == 0 {
      attach.await
    } else {
      tokio::time::timeout_at(started + std::time::Duration::from_millis(timeout_ms), attach)
        .await
        .unwrap_or_else(|_| Err(FerriError::timeout("connecting WebDriver BiDi session", timeout_ms)))
    };
    match result {
      Ok(browser) => Ok(browser),
      Err(error) => {
        if let Err(cleanup) = owner.close().await {
          return Err(FerriError::backend(format!("{error}; cleanup failed: {cleanup}")));
        }
        Err(error)
      },
    }
  }

  pub(crate) async fn from_created(
    created: crate::backend::webdriver::session::CreatedSession,
    endpoint: &str,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<Self> {
    let ws_url = created
      .capabilities
      .get("webSocketUrl")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| {
        FerriError::unsupported("WebDriver server created a Classic session without a BiDi webSocketUrl capability")
      })?
      .to_owned();
    let socket_headers = headers.filter(|_| super::webdriver::same_origin(endpoint, &ws_url));
    let owner = created.session;
    let session = Arc::new(
      BidiSession::connect_existing(
        &ws_url,
        created.id,
        created.capabilities,
        socket_headers,
        Arc::clone(&owner),
      )
      .await?,
    );
    let mut browser = Self::from_session(session)?;
    browser.webdriver = Some(owner);
    Ok(browser)
  }

  fn from_session(session: Arc<BidiSession>) -> Result<Self> {
    let downloads_dir = new_downloads_dir()?;
    let create_ledger = Arc::new(crate::backend::CreateLedger::default());
    let (popup_taps, popups) = Self::spawn_popup_listener(&session, &downloads_dir, &create_ledger);
    Ok(Self {
      session,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      popup_taps,
      create_ledger,
      popups,
      profile_dir: None,
      downloads_dir,
      webdriver: None,
    })
  }

  pub(crate) fn popup_taps(&self) -> crate::backend::PopupTaps {
    Arc::clone(&self.popup_taps)
  }

  /// Resolves once every popup the browser announced before this call has
  /// been registered. Events and replies share one ordered connection, so
  /// the reply to a command comes after the creation event of any context
  /// the browser made before answering it.
  pub(crate) async fn popups_settled(&self) -> Result<()> {
    self
      .session
      .transport
      .send(&ferridriver_bidi_proto::session::Status {})
      .await?;
    self.popups.settled().await;
    Ok(())
  }

  /// Watch `browsingContext.contextCreated` for TOP-LEVEL contexts the
  /// browser created on its own — `window.open` / `target=_blank`
  /// popups carry `originalOpener` (the opener's navigable id), while
  /// contexts our `new_page` creates via `browsingContext.create`
  /// never do. Mirrors Playwright's
  /// `bidiBrowser.ts::_onBrowsingContextCreated`, which resolves the
  /// opener from the same field. A top-level context with no opener that
  /// no create of ours made is another client's page on a shared session,
  /// adopted the same way once the ledger says it is not ours.
  fn spawn_popup_listener(
    session: &Arc<BidiSession>,
    downloads_dir: &Arc<tempfile::TempDir>,
    ledger: &Arc<crate::backend::CreateLedger<ParkedContext>>,
  ) -> (crate::backend::PopupTaps, crate::backend::PopupWatch) {
    let taps: crate::backend::PopupTaps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (popups, mut passes) = crate::backend::PopupWatch::with_barrier();
    let in_flight = popups.in_flight.clone();
    let navigations = super::frames::FirstNavigations::default();
    session
      .transport
      .observe_events(super::frames::first_navigation_observer(Arc::clone(&navigations)));
    let mut rx = session.transport.tap_events();
    let mut flushed = ledger.take_flush_rx();
    let ledger = Arc::clone(ledger);
    let session = Arc::clone(session);
    let adopter = PopupAdopter {
      session: Arc::clone(&session),
      downloads_dir: Arc::clone(downloads_dir),
      navigations,
      taps: Arc::clone(&taps),
    };
    tokio::spawn(async move {
      // This loop is the one place that sees a popup created and then its
      // first request, in that order, so it is what hands a popup's
      // blocked requests to the popup, and what lets through a request
      // only the session intercept blocked on a page with no say in it.
      let mut popups: PopupRequests = rustc_hash::FxHashMap::default();
      let mut parents: rustc_hash::FxHashMap<String, String> = rustc_hash::FxHashMap::default();
      loop {
        // Events first: a barrier pass is answered only once every event
        // queued ahead of it has been handled, which is what lets a caller
        // know each popup announced so far is counted in flight.
        let event = tokio::select! {
          biased;
          event = rx.recv() => match event {
            Some(event) => event,
            None => break,
          },
          Some(()) = flushed.recv() => {
            for (context_id, user_context, claim) in ledger.take_unowned_parked() {
              adopter.adopt(&mut popups, &context_id, user_context, None, claim);
            }
            continue;
          },
          Some(pass) = passes.recv() => {
            if pass.send(()).is_err() {
              debug!("popup barrier caller went away");
            }
            continue;
          },
        };
        match event.method.as_str() {
          "browsingContext.contextCreated" => {},
          "browsingContext.contextDestroyed" => {
            if let Some(context) = event.params.get("context").and_then(|v| v.as_str()) {
              popups.remove(context);
              parents.remove(context);
            }
            continue;
          },
          "network.beforeRequestSent" => {
            Self::forward_blocked_request(&session, &popups, &parents, event);
            continue;
          },
          _ => continue,
        }
        let Some(context_id) = event.params.get("context").and_then(|v| v.as_str()) else {
          continue;
        };
        if let Some(parent) = event.params.get("parent").and_then(|v| v.as_str()) {
          parents.insert(context_id.to_string(), parent.to_string());
          continue;
        }
        let user_context = event
          .params
          .get("userContext")
          .and_then(|v| v.as_str())
          .unwrap_or("default")
          .to_string();
        let claim = in_flight.claim();
        if let Some(opener) = event.params.get("originalOpener").and_then(|v| v.as_str()) {
          adopter.adopt(&mut popups, context_id, user_context, Some(opener), claim);
        } else {
          let parked = (context_id.to_string(), user_context.clone(), Arc::clone(&claim));
          if ledger.try_claim_ownerless(context_id, parked) {
            adopter.adopt(&mut popups, context_id, user_context, None, claim);
          }
        }
      }
    });
    (taps, popups)
  }

  /// Route one blocked request: to the popup whose page it belongs to,
  /// or straight through when only the session intercept blocked it and
  /// the page it belongs to resolves nothing but its own intercept's.
  fn forward_blocked_request(
    session: &Arc<BidiSession>,
    popups: &PopupRequests,
    parents: &rustc_hash::FxHashMap<String, String>,
    event: Arc<super::transport::BidiEvent>,
  ) {
    if event.params.get("isBlocked").and_then(serde_json::Value::as_bool) != Some(true) {
      return;
    }
    let mut top = event.params.get("context").and_then(|v| v.as_str()).unwrap_or("");
    while let Some(parent) = parents.get(top) {
      top = parent;
    }
    if let Some(popup) = popups.get(top) {
      let _ = popup.send(event);
      return;
    }
    let intercepts: Vec<&str> = event
      .params
      .get("intercepts")
      .and_then(serde_json::Value::as_array)
      .map(|ids| ids.iter().filter_map(serde_json::Value::as_str).collect())
      .unwrap_or_default();
    if !session.scopes.blocked_by_session_only(&intercepts) {
      return;
    }
    let Some(request) = event.params.pointer("/request/request").and_then(|v| v.as_str()) else {
      return;
    };
    let session = Arc::clone(session);
    let request = request.to_string();
    tokio::spawn(async move {
      let _ = session
        .transport
        .send(&ferridriver_bidi_proto::network::ContinueRequest {
          request: &request,
          body: None,
          headers: None,
          method: None,
          url: None,
        })
        .await;
    });
  }

  /// Create a new isolated user context. `proxy` is wired via
  /// `browser.createUserContext({ proxy })` — `BiDi`'s proxy shape
  /// matches `WebDriver`'s capabilities (`proxyType: 'manual', httpProxy,
  /// sslProxy, socksProxy, socksVersion, noProxy`). For the common
  /// `http://host:port` / `socks5://host:port` input we decompose into
  /// the equivalent `BiDi` shape.
  ///
  /// `acceptInsecureCerts` is always sent: left out, a user context
  /// inherits the session's, which accepts them so that the default
  /// context can serve `ignoreHTTPSErrors`.
  ///
  /// # Errors
  ///
  /// Returns an error when the `browser.createUserContext` command fails, and
  /// `FerriError::Protocol` when its result has no `userContext` id.
  pub async fn new_context(
    &self,
    proxy: Option<&crate::options::ProxyConfig>,
    accept_insecure_certs: bool,
  ) -> Result<String> {
    let result = self
      .session
      .transport
      .send(&ferridriver_bidi_proto::browser::CreateUserContext {
        accept_insecure_certs: Some(accept_insecure_certs),
        proxy: proxy.map(bidi_proxy_capability),
      })
      .await?;
    Ok(result.user_context)
  }

  /// Dispose an isolated user context.
  ///
  /// # Errors
  ///
  /// Returns an error when the `browser.removeUserContext` command fails, for example for an
  /// unknown user context. Failures listing the context's pages or waiting for them to close are
  /// ignored.
  pub async fn dispose_context(&self, user_context_id: &str) -> Result<()> {
    let context_ids = self
      .list_context_ids_for_user_context(user_context_id)
      .await
      .unwrap_or_default();
    let mut waiters = Vec::with_capacity(context_ids.len());
    for context_id in &context_ids {
      waiters.push(self.wait_for_context_event(
        "browsingContext.contextDestroyed",
        context_id,
        std::time::Duration::from_secs(2),
      ));
    }
    self
      .session
      .transport
      .send(&ferridriver_bidi_proto::browser::RemoveUserContext {
        user_context: user_context_id,
      })
      .await?;
    let _ = futures::future::join_all(waiters).await;
    Ok(())
  }

  /// List the open pages (top-level browsing contexts) of the default user
  /// context. A user context another client created holds that client's
  /// pages, which are left alone.
  ///
  /// # Errors
  ///
  /// Returns an error when the `browsingContext.getTree` command fails, and `FerriError::Protocol`
  /// when its result lacks the contexts array or a context id.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    let result = self
      .session
      .transport
      .send(&ferridriver_bidi_proto::browsing_context::GetTree {
        max_depth: None,
        root: None,
      })
      .await?;

    let mut pages = Vec::with_capacity(result.contexts.len());
    for ctx in result.contexts {
      let user_context = ctx.user_context.as_deref();
      if user_context.is_some_and(|id| id != "default") {
        continue;
      }
      pages.push(AnyPage::Bidi(BidiPage::create(
        Arc::clone(&self.session),
        ctx.context,
        user_context,
        Arc::clone(&self.downloads_dir),
      )));
    }
    Ok(pages)
  }

  /// Create a new page (tab) and optionally navigate.
  ///
  /// # Errors
  ///
  /// Returns an error when the `browsingContext.create` command fails or returns no context id, or
  /// when waiting for the page to be ready, emulating `viewport`, or navigating to `url` fails,
  /// including a timeout when that load takes longer than 30 seconds.
  pub async fn new_page(
    &self,
    url: &str,
    user_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    let mut rx = self.session.transport.tap_events();
    self.create_ledger.begin_create();
    let created = self
      .session
      .transport
      .send(&ferridriver_bidi_proto::browsing_context::Create {
        kind: ferridriver_bidi_proto::browsing_context::CreateType::Window,
        reference_context: None,
        background: None,
        user_context: user_context_id,
      })
      .await
      .map(|result| result.context);
    self.create_ledger.end_create(created.as_deref().ok());
    let context_id = created?;

    let wait_for_created = async {
      while let Some(event) = rx.recv().await {
        if event.method != "browsingContext.contextCreated" {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(&context_id) {
          return Ok::<(), String>(());
        }
      }
      Err("BiDi event stream closed while waiting for browsingContext.contextCreated".to_string())
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), wait_for_created).await;

    debug!("BiDi new page: context={context_id}");
    let page = BidiPage::create(
      Arc::clone(&self.session),
      context_id,
      user_context_id,
      Arc::clone(&self.downloads_dir),
    );
    // Wait for the initial about:blank browsing context to settle before
    // applying the viewport: on Windows Firefox, `browsingContext.setViewport`
    // raced against the initial context swap can fail with
    // `DiscardedBrowsingContextError`.
    page.wait_until_ready().await?;
    if let Some(viewport) = viewport {
      page.emulate_viewport(viewport).await?;
    }

    if !url.is_empty() && url != "about:blank" {
      page.goto(url, NavLifecycle::Load, 30_000, None).await?;
    }

    Ok(AnyPage::Bidi(page))
  }

  /// Close the browser.
  ///
  /// Caller-owned profiles flush through `browser.close`; throwaway
  /// profiles can be killed directly because they are discarded.
  ///
  /// # Errors
  ///
  /// Returns an error when deleting the Classic `WebDriver` session fails, when Firefox with a
  /// caller-owned profile does not exit within the flush timeout after `browser.close`, or when
  /// closing the `BiDi` transport fails.
  pub async fn close(&self) -> Result<()> {
    if let Some(owner) = &self.webdriver {
      let result = owner.close().await;
      self.session.transport.start_close();
      result?;
    }
    let mut child = self.child.lock().await;
    if let Some(group) = child.as_mut() {
      let mut flushed = Ok(());
      if self.profile_dir.is_none() && self.webdriver.is_none() {
        let timeout = crate::backend::process::PROFILE_FLUSH_TIMEOUT;
        let _ = tokio::time::timeout(
          timeout,
          self.session.transport.send(&ferridriver_bidi_proto::browser::Close {}),
        )
        .await;
        flushed = group.wait_for_exit(timeout).await;
      }
      // Flag the transport first: the SIGKILLed Firefox never sends a
      // WebSocket close frame, and without the flag the reader logs the
      // resulting TCP reset as a spurious WARN on every teardown.
      self.session.transport.start_close();
      // Group kill first (helpers die with the parent), then reap so
      // the enclosing runtime carries no zombie.
      group.shutdown_for_close().await;
      child.take();
      if let Err(reason) = flushed {
        return Err(FerriError::Backend(format!(
          "Firefox did not close cleanly while flushing its persistent profile: {reason}"
        )));
      }
    }
    drop(child);
    // As Playwright's `processLauncher`: a profile file something still
    // holds after the retries (Windows) is logged and left to the next
    // attempt when the directory is dropped, not reported as a failed close.
    if let Some(dir) = self.profile_dir.as_ref() {
      let _ = dir.remove_now().await;
    }
    self.session.transport.close().await
  }

  /// Whether the launched Firefox is still running. `None` when this
  /// handle connected to a browser it did not launch.
  pub(crate) fn child_is_running(&self) -> Option<bool> {
    let mut guard = self.child.try_lock().ok()?;
    guard.as_mut().map(crate::backend::process::ChildGroup::is_running)
  }
}

pub(crate) use crate::backend::webdriver::session_url as webdriver_session_url;

pub(crate) use crate::backend::common::webdriver_capabilities;

/// A Playwright-shaped proxy as the `BiDi`/`WebDriver` `proxy` capability.
///
/// The same shape serves `session.new` (browser-wide, from
/// `launch({ proxy })`) and `browser.createUserContext` (per context).
pub(crate) fn bidi_proxy_capability(proxy: &crate::options::ProxyConfig) -> serde_json::Value {
  let (proxy_type, host_port, is_socks, socks_version) = parse_bidi_proxy(&proxy.server_origin());
  let mut capability = json!({ "proxyType": proxy_type });

  if is_socks {
    capability["socksProxy"] = json!(host_port);
    if let Some(version) = socks_version {
      capability["socksVersion"] = json!(version);
    }
  } else {
    capability["httpProxy"] = json!(host_port);
    capability["sslProxy"] = json!(host_port);
  }

  if let Some(ref bypass) = proxy.bypass {
    // WebDriver noProxy is an array of host strings.
    let list: Vec<&str> = bypass.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    capability["noProxy"] = json!(list);
  }

  capability
}

/// Decompose a Playwright-shaped proxy `server` string into the
/// BiDi/WebDriver capability fields. Returns `(proxyType, host_port,
/// is_socks, socks_version)`.
fn parse_bidi_proxy(server: &str) -> (&'static str, String, bool, Option<i64>) {
  let (scheme, rest) = server.split_once("://").unwrap_or(("", server));
  let host_port = rest.to_string();
  match scheme {
    "socks5" | "socks5h" => ("manual", host_port, true, Some(5)),
    "socks4" | "socks4a" => ("manual", host_port, true, Some(4)),
    _ => ("manual", host_port, false, None),
  }
}

/// One temp downloads directory per browser. `Playwright.setDownloadBehavior`
/// equivalents are per browser too, and download filenames are unique.
fn new_downloads_dir() -> Result<Arc<tempfile::TempDir>> {
  Ok(Arc::new(
    tempfile::Builder::new()
      .prefix("ferridriver-downloads-")
      .tempdir()
      .map_err(|e| FerriError::backend(format!("downloads tempdir: {e}")))?,
  ))
}

#[cfg(test)]
mod proxy_capability_tests {
  use super::bidi_proxy_capability;
  use crate::options::ProxyConfig;

  #[test]
  fn an_http_proxy_becomes_a_manual_webdriver_capability() {
    let capability = bidi_proxy_capability(&ProxyConfig {
      server: "http://127.0.0.1:3052".to_string(),
      bypass: Some("127.0.0.1, localhost".to_string()),
      username: None,
      password: None,
    });

    assert_eq!(capability["proxyType"], "manual");
    // WebDriver wants host:port, not a URL, and the same value for both schemes.
    assert_eq!(capability["httpProxy"], "127.0.0.1:3052");
    assert_eq!(capability["sslProxy"], "127.0.0.1:3052");
    assert_eq!(capability["noProxy"], serde_json::json!(["127.0.0.1", "localhost"]));
  }

  #[test]
  fn a_socks_proxy_carries_its_version() {
    let capability = bidi_proxy_capability(&ProxyConfig {
      server: "socks5://127.0.0.1:1080".to_string(),
      bypass: None,
      username: None,
      password: None,
    });

    assert_eq!(capability["socksProxy"], "127.0.0.1:1080");
    assert_eq!(capability["socksVersion"], 5);
    assert!(
      capability.get("httpProxy").is_none(),
      "a socks proxy is not an http one"
    );
  }

  #[test]
  fn socks5h_is_socks_version_5_not_an_http_proxy() {
    let capability = bidi_proxy_capability(&ProxyConfig {
      server: "socks5h://127.0.0.1:1080".to_string(),
      bypass: None,
      username: None,
      password: None,
    });

    assert_eq!(capability["socksProxy"], "127.0.0.1:1080");
    assert_eq!(capability["socksVersion"], 5);
    assert!(capability.get("httpProxy").is_none());
  }
}

#[cfg(test)]
mod webdriver_url_tests {
  use super::{BidiBrowser, webdriver_capabilities, webdriver_session_url};

  #[tokio::test]
  async fn classic_only_session_is_deleted_before_connect_returns() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for body in [
        r#"{"value":{"sessionId":"mobile-contract","capabilities":{"browserName":"safari"}}}"#,
        r#"{"value":null}"#,
      ] {
        let Ok(Ok((mut socket, _))) = tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await
        else {
          break;
        };
        let mut request = vec![0; 8192];
        let length = socket.read(&mut request).await.unwrap();
        requests.push(String::from_utf8(request[..length].to_vec()).unwrap());
        let response = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
      }
      requests
    });
    let mut headers = rustc_hash::FxHashMap::default();
    headers.insert("authorization".into(), "Bearer contract-token".into());
    let result = BidiBrowser::connect_webdriver(
      &format!("http://{address}/wd/hub"),
      "safari",
      None,
      Some(&headers),
      Some(1000),
    )
    .await;
    assert!(matches!(result, Err(crate::error::FerriError::Unsupported(_))));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2, "created remote session was abandoned");
    assert!(requests[1].starts_with("DELETE /wd/hub/session/mobile-contract HTTP/1.1\r\n"));
    assert!(
      requests[1]
        .to_ascii_lowercase()
        .contains("authorization: bearer contract-token")
    );
  }

  #[test]
  fn appends_session_to_server_root() {
    assert_eq!(
      webdriver_session_url("http://127.0.0.1:4444/").unwrap().as_str(),
      "http://127.0.0.1:4444/session"
    );
  }

  #[test]
  fn preserves_existing_session_path() {
    assert_eq!(
      webdriver_session_url("http://127.0.0.1:4444/wd/hub/session")
        .unwrap()
        .as_str(),
      "http://127.0.0.1:4444/wd/hub/session"
    );
  }

  #[test]
  fn rejects_invalid_endpoint() {
    assert!(webdriver_session_url("not a url").is_err());
    assert!(webdriver_session_url("file:///tmp/driver").is_err());
  }

  #[test]
  fn trailing_session_slash_does_not_create_a_second_session_segment() {
    assert_eq!(
      webdriver_session_url("https://example.com/wd/hub/session/?region=eu#ignored")
        .unwrap()
        .as_str(),
      "https://example.com/wd/hub/session?region=eu"
    );
  }

  #[test]
  fn merges_vendor_capabilities_without_dropping_bidi_negotiation() {
    let caps = webdriver_capabilities(
      "safari",
      Some(&serde_json::json!({
        "platformName": "ios",
        "appium:options": { "automationName": "Safari" },
        "webSocketUrl": false
      })),
    );
    assert_eq!(caps["browserName"], "safari");
    assert_eq!(caps["platformName"], "ios");
    assert_eq!(caps["appium:options"]["automationName"], "Safari");
    assert_eq!(caps["webSocketUrl"], true);
  }

  #[tokio::test]
  async fn webdriver_headers_reach_the_session_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock WebDriver");
    let address = listener.local_addr().expect("mock address");
    let server = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.expect("accept session request");
      let mut request = Vec::new();
      let mut chunk = [0; 1024];
      loop {
        let read = socket.read(&mut chunk).await.expect("read session request");
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") || read == 0 {
          break;
        }
      }
      let body = br#"{"value":{"error":"session not created","message":"test response"}}"#;
      let response = format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
      );
      socket.write_all(response.as_bytes()).await.expect("write status");
      socket.write_all(body).await.expect("write body");
      String::from_utf8_lossy(&request).into_owned()
    });

    let mut headers = rustc_hash::FxHashMap::default();
    headers.insert("authorization".to_string(), "Bearer test-token".to_string());
    let result = Box::pin(BidiBrowser::connect_webdriver(
      &format!("http://{address}"),
      "safari",
      Some(&serde_json::json!({"platformName": "ios"})),
      Some(&headers),
      Some(1_000),
    ))
    .await;
    let Err(error) = result else {
      panic!("the mock endpoint rejects the session");
    };
    assert!(error.to_string().contains("500 Internal Server Error"));
    let request = server.await.expect("mock server task");
    assert!(
      request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-token")
    );
    assert!(request.contains("\"platformName\":\"ios\""));
    assert!(request.contains("\"webSocketUrl\":true"));
  }
}
