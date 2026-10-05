//! [`BrowserDispatcher`]: runs scripts against a live [`ferridriver::Browser`].
//!
//! This is the [`crate::Dispatcher`] a bound browser runs, and its whole
//! surface is one verb: [`crate::protocol::RUN_VERB`]. Everything a client
//! wants to do — snapshot, click, read state, mock a route, drive a whole
//! flow — is a script, so the protocol carries a script rather than a table of
//! verbs that would forever lag the scripting API behind it.
//!
//! Running the script is delegated to a [`ScriptHost`] supplied by a higher
//! crate, because the scripting engine lives above this crate in the
//! dependency graph.

use std::sync::Arc;

use async_trait::async_trait;
use ferridriver::state::{BrowserState, SessionKey};
use ferridriver::{Browser, Page};
use tokio::sync::RwLock;

use crate::dispatch::{Dispatcher, EventSink, ScriptHost};
use crate::protocol::{Command, RUN_VERB, Response, ScriptRequest};

/// Runs session commands against a live browser.
pub struct BrowserDispatcher {
  browser: Browser,
  browser_name: String,
  script_host: Option<Arc<dyn ScriptHost>>,
}

impl BrowserDispatcher {
  /// Build a dispatcher retaining the bound browser's product identity.
  #[must_use]
  pub fn new(browser: &Browser) -> Self {
    Self {
      browser: browser.clone(),
      browser_name: browser_name_for(browser).to_owned(),
      script_host: None,
    }
  }

  /// Register the script host. Without it the session has no usable surface,
  /// so every bind path installs one; a dispatcher without a host answers
  /// every command with a "scripting is not available" error rather than
  /// pretending to work.
  #[must_use]
  pub fn with_script_host(mut self, host: Arc<dyn ScriptHost>) -> Self {
    self.script_host = Some(host);
    self
  }

  /// The browser-engine name for the registry descriptor.
  #[must_use]
  pub fn browser_name(&self) -> &str {
    &self.browser_name
  }

  /// The shared browser state this dispatcher drives.
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    self.browser.state()
  }

  fn context_of(command: &Command) -> &str {
    command.context.as_deref().unwrap_or("default")
  }
}

#[async_trait]
impl Dispatcher for BrowserDispatcher {
  async fn dispatch(&self, command: Command, events: EventSink) -> Response {
    if command.verb != RUN_VERB {
      return Response::err(
        command.id,
        format!(
          "unknown command verb '{}': a bound browser accepts '{RUN_VERB}'",
          command.verb
        ),
      );
    }
    let Some(host) = &self.script_host else {
      return Response::err(command.id, "scripting is not available on this session server");
    };
    let request: ScriptRequest = match serde_json::from_value(command.args.clone()) {
      Ok(r) => r,
      Err(e) => return Response::err(command.id, format!("malformed run request: {e}")),
    };
    match host.run(Self::context_of(&command), request, events).await {
      Ok(value) => Response::ok_json(command.id, &value),
      Err(msg) => Response::err(command.id, msg),
    }
  }

  async fn close(&self) -> std::result::Result<(), String> {
    let release = Box::pin(async { self.browser.close().await.map_err(|error| error.to_string()) });
    match &self.script_host {
      Some(host) => host.close(release).await,
      None => release.await,
    }
  }

  fn verbs(&self) -> Vec<&'static str> {
    vec![RUN_VERB]
  }
}

/// Normalize the reported browser product for the registry descriptor.
#[must_use]
pub fn browser_name_for(browser: &Browser) -> &str {
  let product = browser.version().split('/').next().unwrap_or(browser.version());
  if ["Chrome", "HeadlessChrome", "Chromium"]
    .iter()
    .any(|name| product.eq_ignore_ascii_case(name))
  {
    "chromium"
  } else if product.eq_ignore_ascii_case("firefox") {
    "firefox"
  } else if product.eq_ignore_ascii_case("safari") {
    "safari"
  } else if ["webkit", "webkit-playwright"]
    .iter()
    .any(|name| product.eq_ignore_ascii_case(name))
  {
    "webkit"
  } else {
    product
  }
}

/// Build a dispatcher straight from a [`Browser`] handle, reading its backend
/// identity and sharing its state. The most common construction path for a host
/// that already holds a `Browser`.
#[must_use]
pub fn dispatcher_for(browser: &Browser) -> BrowserDispatcher {
  BrowserDispatcher::new(browser)
}

/// Resolve a context name to a live `Page` on `state`, opening one on first
/// use. Shared by every script host so "the session's page" means the same
/// thing regardless of which crate asks.
///
/// # Errors
///
/// Returns whatever launching the instance or opening the page failed with.
pub async fn page_for(browser: &Browser, context: &str) -> ferridriver::Result<Arc<Page>> {
  let state = browser.state();
  let key = context_key_for(browser, context)?;
  let context = key.to_composite();
  let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(state), context.clone()).with_browser(browser.clone());
  {
    let guard = state.read().await;
    if let Ok(any_page) = guard.active_page(&context) {
      let any_page = any_page.clone();
      return Ok(Page::with_context(any_page, ctx_ref));
    }
  }
  Box::pin(ctx_ref.new_page()).await
}

/// # Errors
/// Rejects a qualified context that belongs to a different bound browser instance.
pub fn context_key_for(browser: &Browser, context: &str) -> ferridriver::Result<SessionKey> {
  let mut key = SessionKey::parse(&browser.default_context().composite());
  if context.contains(':') {
    let requested = SessionKey::parse(context);
    if requested.instance != key.instance {
      return Err(ferridriver::FerriError::invalid_argument(
        "context",
        "context belongs to another browser instance",
      ));
    }
    return Ok(requested);
  }
  key.context = Arc::from(context);
  Ok(key)
}

/// Parse a session key into its `instance:context` halves. Re-exported so the
/// CLI and hosts share ferridriver core's parsing.
///
/// Vocabulary-free: a bare name is a CONTEXT. The session CLI addresses
/// browsers it bound itself and has no config document declaring instance
/// names — a host that does have one resolves keys through
/// `BrowserState::session_key` instead.
#[must_use]
pub fn parse_session_key(s: &str) -> SessionKey {
  SessionKey::parse(s)
}
