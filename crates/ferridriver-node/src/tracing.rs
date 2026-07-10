//! NAPI binding for `context.tracing`.
//!
//! Mirrors Playwright's `Tracing` (`client/tracing.ts`). HAR recording
//! (`startHar` / `stopHar`, 1.60) is fully wired; the trace `.zip`
//! recorder (`start` / `stop` / `startChunk` / `stopChunk`) surfaces the
//! core's typed Unsupported error.

use ferridriver::tracing::{HarContentPolicy, HarMode, StartHarOptions};
use ferridriver::url_matcher::UrlMatcher;
use napi::Result;
use napi::bindgen_prelude::Either;
use napi_derive::napi;

use crate::error::IntoNapi;

/// Options bag for `tracing.startHar`.
#[napi(object)]
pub struct StartHarOptionsJs {
  /// `'embed' | 'attach' | 'omit'`.
  pub content: Option<String>,
  /// `'full' | 'minimal'`.
  pub mode: Option<String>,
  /// Only record requests whose URL matches this glob or `RegExp`.
  pub url_filter: Option<Either<String, crate::types::JsRegExpLike>>,
  /// Where `attach`ed bodies are written for a non-zip HAR. Incompatible
  /// with a `.zip` path.
  pub resources_dir: Option<String>,
}

/// `context.tracing` — Playwright's `Tracing`.
#[napi]
pub struct Tracing {
  inner: ferridriver::ContextRef,
}

impl Tracing {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self { inner }
  }
}

#[napi]
impl Tracing {
  /// Playwright: `tracing.startHar(path, { content?, mode?, urlFilter?, resourcesDir? })`.
  /// Records this context's network into a HAR file until `stopHar`. A
  /// `.zip` path packs `har.har` plus attached bodies; default content
  /// policy is `attach` for `.zip`, `embed` otherwise.
  #[napi(
    ts_args_type = "path: string, options?: { content?: 'embed' | 'attach' | 'omit', mode?: 'full' | 'minimal', urlFilter?: string | RegExp, resourcesDir?: string }"
  )]
  pub async fn start_har(&self, path: String, options: Option<StartHarOptionsJs>) -> Result<()> {
    let opts = build_start_har_options(options)?;
    self.inner.tracing().start_har(path, opts).await.into_napi()
  }

  /// Playwright: `tracing.stopHar()`. Writes the recorded HAR to disk.
  #[napi]
  pub async fn stop_har(&self) -> Result<()> {
    self.inner.tracing().stop_har().await.into_napi()
  }

  /// Playwright: `tracing.start(options?)`. Trace `.zip` recording is not
  /// implemented (returns the core Unsupported error).
  #[napi]
  pub async fn start(&self) -> Result<()> {
    self.inner.tracing().start().await.into_napi()
  }

  /// Playwright: `tracing.startChunk(options?)`. Not implemented.
  #[napi]
  pub async fn start_chunk(&self) -> Result<()> {
    self.inner.tracing().start_chunk().await.into_napi()
  }

  /// Playwright: `tracing.stopChunk(options?)`. Not implemented.
  #[napi]
  pub async fn stop_chunk(&self) -> Result<()> {
    self.inner.tracing().stop_chunk().await.into_napi()
  }

  /// Playwright: `tracing.stop(options?)`. Not implemented.
  #[napi]
  pub async fn stop(&self) -> Result<()> {
    self.inner.tracing().stop().await.into_napi()
  }
}

fn build_start_har_options(options: Option<StartHarOptionsJs>) -> Result<StartHarOptions> {
  let url_filter = options.as_ref().and_then(|o| o.url_filter.as_ref());
  let url_filter = match url_filter {
    Some(Either::A(glob)) => Some(UrlMatcher::glob(glob.clone()).into_napi()?),
    Some(Either::B(re)) => {
      Some(UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or("")).into_napi()?)
    },
    None => None,
  };
  let content = match options.as_ref().and_then(|o| o.content.as_deref()) {
    Some("embed") => Some(HarContentPolicy::Embed),
    Some("attach") => Some(HarContentPolicy::Attach),
    Some("omit") => Some(HarContentPolicy::Omit),
    Some(other) => return Err(napi::Error::from_reason(format!("invalid HAR content policy: {other}"))),
    None => None,
  };
  let mode = match options.as_ref().and_then(|o| o.mode.as_deref()) {
    Some("full") => Some(HarMode::Full),
    Some("minimal") => Some(HarMode::Minimal),
    Some(other) => return Err(napi::Error::from_reason(format!("invalid HAR mode: {other}"))),
    None => None,
  };
  Ok(StartHarOptions {
    content,
    mode,
    url_filter,
    resources_dir: options
      .as_ref()
      .and_then(|o| o.resources_dir.as_ref())
      .map(std::path::PathBuf::from),
  })
}
