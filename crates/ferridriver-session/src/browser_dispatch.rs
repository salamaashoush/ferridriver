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
use ferridriver::backend::BackendKind;
use ferridriver::state::{BrowserState, SessionKey};
use ferridriver::{Browser, Page};
use tokio::sync::RwLock;

use crate::dispatch::{Dispatcher, EventSink, ScriptHost};
use crate::protocol::{Command, RUN_VERB, Response, ScriptRequest};

/// Runs session commands against a live browser.
pub struct BrowserDispatcher {
  state: Arc<RwLock<BrowserState>>,
  backend: BackendKind,
  script_host: Option<Arc<dyn ScriptHost>>,
}

impl BrowserDispatcher {
  /// Build a dispatcher over the given shared browser state.
  #[must_use]
  pub fn new(state: Arc<RwLock<BrowserState>>, backend: BackendKind) -> Self {
    Self {
      state,
      backend,
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
  pub fn browser_name(&self) -> &'static str {
    browser_name_for(self.backend)
  }

  /// The shared browser state this dispatcher drives.
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    &self.state
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

  fn verbs(&self) -> Vec<&'static str> {
    vec![RUN_VERB]
  }
}

/// Resolve the browser-engine name for a [`SessionKey`]'s instance from a
/// backend kind. Used by [`crate::bind()`] to fill the registry descriptor.
#[must_use]
pub fn browser_name_for(backend: BackendKind) -> &'static str {
  match backend {
    BackendKind::Bidi => "firefox",
    BackendKind::WebKit => "webkit",
    _ => "chromium",
  }
}

/// Build a dispatcher straight from a [`Browser`] handle, reading its backend
/// kind and sharing its state. The most common construction path for a host
/// that already holds a `Browser`.
#[must_use]
pub fn dispatcher_for(browser: &Browser) -> BrowserDispatcher {
  BrowserDispatcher::new(Arc::clone(browser.state()), browser.backend_kind())
}

/// Resolve a context name to a live `Page` on `state`, opening one on first
/// use. Shared by every script host so "the session's page" means the same
/// thing regardless of which crate asks.
///
/// # Errors
///
/// Returns whatever launching the instance or opening the page failed with.
pub async fn page_for(state: &Arc<RwLock<BrowserState>>, context: &str) -> ferridriver::Result<Arc<Page>> {
  {
    let guard = state.read().await;
    if let Ok(any_page) = guard.active_page(context) {
      let any_page = any_page.clone();
      let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(state), context.to_string());
      return Ok(Page::with_context(any_page, ctx_ref));
    }
  }
  let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(state), context.to_string());
  Box::pin(ctx_ref.new_page()).await
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
