//! [`BrowserDispatcher`]: maps session command verbs onto a live
//! [`ferridriver::Browser`].
//!
//! This is the [`crate::Dispatcher`] a bound browser runs. Every verb is a
//! thin wrapper over core `Page` / `actions` calls, so it works on all four
//! backends with no per-backend code here. Verbs that read or act on a
//! specific element accept either a CSS `selector` or a `ref` from the most
//! recent `snapshot` (refs are stored per context, exactly like the MCP
//! server's snapshot bridge).
//!
//! The `run_script` verb is delegated to an optional [`ScriptHook`] supplied
//! by a higher crate, because the scripting engine lives above this crate in
//! the dependency graph.

use std::sync::Arc;

use async_trait::async_trait;
use ferridriver::backend::BackendKind;
use ferridriver::state::{BrowserState, SessionKey};
use ferridriver::{Browser, Page};
use rustc_hash::FxHashMap;
use tokio::sync::RwLock;

use crate::dispatch::{Dispatcher, ScriptHook};
use crate::protocol::{Command, Response};

/// The set of verbs [`BrowserDispatcher`] understands. Kept as one list so
/// `help` and the CLI share a single source of truth.
pub const BROWSER_VERBS: &[&str] = &[
  "snapshot",
  "goto",
  "back",
  "forward",
  "reload",
  "click",
  "fill",
  "press",
  "hover",
  "eval",
  "screenshot",
  "title",
  "url",
  "run-script",
];

/// Maps session commands onto a live browser, resolving each command's target
/// context to a `Page` on demand.
pub struct BrowserDispatcher {
  state: Arc<RwLock<BrowserState>>,
  backend: BackendKind,
  /// Most recent snapshot ref-map per context, populated by the `snapshot`
  /// verb and consumed by element verbs that pass a `ref`.
  ref_maps: Arc<RwLock<FxHashMap<String, FxHashMap<String, i64>>>>,
  /// Optional `run_script` handler from a higher crate.
  script_hook: Option<Arc<dyn ScriptHook>>,
}

impl BrowserDispatcher {
  /// Build a dispatcher over the given shared browser state.
  #[must_use]
  pub fn new(state: Arc<RwLock<BrowserState>>, backend: BackendKind) -> Self {
    Self {
      state,
      backend,
      ref_maps: Arc::new(RwLock::new(FxHashMap::default())),
      script_hook: None,
    }
  }

  /// Register the `run_script` handler. Without it, the `run-script` verb
  /// returns a "scripting not available" error.
  #[must_use]
  pub fn with_script_hook(mut self, hook: Arc<dyn ScriptHook>) -> Self {
    self.script_hook = Some(hook);
    self
  }

  /// The browser-engine name for the registry descriptor.
  #[must_use]
  pub fn browser_name(&self) -> &'static str {
    match self.backend {
      BackendKind::Bidi => "firefox",
      BackendKind::WebKit => "webkit",
      _ => "chromium",
    }
  }

  fn context_of(command: &Command) -> &str {
    command.context.as_deref().unwrap_or("default")
  }

  /// Resolve a context name to a live `Page`, launching the instance / opening
  /// a page on first use (mirrors the MCP server's `ensure_active_page`).
  async fn page_for(&self, context: &str) -> ferridriver::Result<Arc<Page>> {
    {
      let state = self.state.read().await;
      if let Ok(any_page) = state.active_page(context) {
        let any_page = any_page.clone();
        let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(&self.state), context.to_string());
        return Ok(Page::with_context(any_page, ctx_ref));
      }
    }
    let ctx_ref = ferridriver::context::ContextRef::new(Arc::clone(&self.state), context.to_string());
    Box::pin(ctx_ref.new_page()).await
  }

  /// Take a snapshot, store its ref-map for `context`, and return the text.
  async fn snapshot(&self, context: &str) -> std::result::Result<String, String> {
    let page = self.page_for(context).await.map_err(|e| e.to_string())?;
    let snap = page.snapshot_for_ai().await.map_err(|e| e.to_string())?;
    self.ref_maps.write().await.insert(context.to_string(), snap.ref_map);
    Ok(snap.full)
  }

  /// Resolve an element from a command's `ref` (against the stored ref-map) or
  /// `selector` and act on it. Returns the resolved [`ferridriver::backend::AnyElement`].
  async fn resolve_element(
    &self,
    context: &str,
    page: &Page,
    args: &serde_json::Value,
  ) -> std::result::Result<ferridriver::backend::AnyElement, String> {
    let r#ref = args.get("ref").and_then(|v| v.as_str());
    let selector = args.get("selector").and_then(|v| v.as_str());
    let maps = self.ref_maps.read().await;
    let empty = FxHashMap::default();
    let ref_map = maps.get(context).unwrap_or(&empty);
    ferridriver::actions::resolve_element(page.inner(), ref_map, r#ref, selector)
      .await
      .map_err(|e| e.to_string())
  }

  /// Dispatch a verb, returning either result text or an error message.
  async fn run_verb(&self, command: &Command) -> std::result::Result<VerbOutput, String> {
    let context = Self::context_of(command);
    let args = &command.args;
    match command.verb.as_str() {
      "snapshot" => Ok(VerbOutput::text(self.snapshot(context).await?)),
      "goto" => {
        let url = str_arg(args, "url")?;
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        page.goto(url).await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(self.snapshot(context).await?))
      },
      "back" => self.navigate(context, Nav::Back).await,
      "forward" => self.navigate(context, Nav::Forward).await,
      "reload" => self.navigate(context, Nav::Reload).await,
      "click" => {
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        let element = self.resolve_element(context, &page, args).await?;
        element.click().await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(self.snapshot(context).await?))
      },
      "fill" => {
        let value = str_arg(args, "value")?;
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        let element = self.resolve_element(context, &page, args).await?;
        ferridriver::actions::fill(&element, page.inner(), value, false)
          .await
          .map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(self.snapshot(context).await?))
      },
      "press" => {
        let selector = str_arg(args, "selector")?;
        let key = str_arg(args, "key")?;
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        page.press(selector, key).await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(self.snapshot(context).await?))
      },
      "hover" => {
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        let element = self.resolve_element(context, &page, args).await?;
        ferridriver::actions::hover_with_opts(&element, page.inner(), &ferridriver::options::HoverOptions::default())
          .await
          .map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(self.snapshot(context).await?))
      },
      "eval" => {
        let expression = str_arg(args, "expression")?;
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        let value = page
          .evaluate(expression, ferridriver::protocol::SerializedArgument::default(), None)
          .await
          .map_err(|e| e.to_string())?;
        let rendered = value.to_json_like().map_or_else(
          || value.as_string_lossy(),
          |v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
        );
        Ok(VerbOutput::text(rendered))
      },
      "screenshot" => {
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        let bytes = page.screenshot().await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::data("captured screenshot", bytes))
      },
      "title" => {
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(page.title().await.map_err(|e| e.to_string())?))
      },
      "url" => {
        let page = self.page_for(context).await.map_err(|e| e.to_string())?;
        Ok(VerbOutput::text(page.url()))
      },
      "run-script" => {
        let Some(hook) = &self.script_hook else {
          return Err("scripting is not available on this session server".to_string());
        };
        let source = str_arg(args, "source")?;
        let script_args: Vec<serde_json::Value> =
          args.get("args").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        Ok(VerbOutput::text(hook.run_script(context, source, &script_args).await?))
      },
      other => Err(format!("unknown command verb: '{other}'")),
    }
  }

  async fn navigate(&self, context: &str, nav: Nav) -> std::result::Result<VerbOutput, String> {
    let page = self.page_for(context).await.map_err(|e| e.to_string())?;
    match nav {
      Nav::Back => page.go_back().await.map_err(|e| e.to_string())?,
      Nav::Forward => page.go_forward().await.map_err(|e| e.to_string())?,
      Nav::Reload => page.reload().await.map_err(|e| e.to_string())?,
    };
    Ok(VerbOutput::text(self.snapshot(context).await?))
  }
}

enum Nav {
  Back,
  Forward,
  Reload,
}

/// What a verb produced before it is folded into a [`Response`].
struct VerbOutput {
  text: String,
  data: Option<Vec<u8>>,
}

impl VerbOutput {
  fn text(text: impl Into<String>) -> Self {
    Self {
      text: text.into(),
      data: None,
    }
  }

  fn data(text: impl Into<String>, bytes: Vec<u8>) -> Self {
    Self {
      text: text.into(),
      data: Some(bytes),
    }
  }
}

#[async_trait]
impl Dispatcher for BrowserDispatcher {
  async fn dispatch(&self, command: Command) -> Response {
    match self.run_verb(&command).await {
      Ok(out) => match out.data {
        Some(bytes) => {
          use base64::Engine;
          let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
          Response::ok_data(command.id, out.text, b64)
        },
        None => Response::ok(command.id, out.text),
      },
      Err(msg) => Response::err(command.id, msg),
    }
  }

  fn verbs(&self) -> Vec<&'static str> {
    BROWSER_VERBS.to_vec()
  }
}

fn str_arg<'a>(args: &'a serde_json::Value, key: &str) -> std::result::Result<&'a str, String> {
  args
    .get(key)
    .and_then(|v| v.as_str())
    .ok_or_else(|| format!("missing required string argument '{key}'"))
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

/// Parse a session key into its `instance:context` halves. Re-exported so the
/// CLI and hosts share ferridriver core's parsing.
#[must_use]
pub fn parse_session_key(s: &str) -> SessionKey {
  SessionKey::parse(s)
}
