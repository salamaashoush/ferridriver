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

  /// Playwright: `tracing.start(options?: { name?, title?, screenshots?,
  /// snapshots?, sources? })`. Records a Playwright-format trace; write
  /// it with `stop({ path })` and open it in `npx playwright show-trace`.
  #[napi(
    ts_args_type = "options?: { name?: string, title?: string, screenshots?: boolean, snapshots?: boolean, sources?: boolean }"
  )]
  pub async fn start(&self, options: Option<TracingStartOptionsJs>) -> Result<()> {
    let opts = options.map(Into::into).unwrap_or_default();
    self.inner.tracing().start(opts).await.into_napi()
  }

  /// Playwright: `tracing.startChunk(options?)`.
  #[napi]
  pub async fn start_chunk(&self) -> Result<()> {
    self.inner.tracing().start_chunk().await.into_napi()
  }

  /// Playwright: `tracing.stopChunk(options?: { path? })`.
  #[napi(ts_args_type = "options?: { path?: string }")]
  pub async fn stop_chunk(&self, options: Option<TracingStopOptionsJs>) -> Result<()> {
    let opts = options.map(Into::into).unwrap_or_default();
    self.inner.tracing().stop_chunk(opts).await.into_napi()
  }

  /// Playwright: `tracing.stop(options?: { path? })`.
  #[napi(ts_args_type = "options?: { path?: string }")]
  pub async fn stop(&self, options: Option<TracingStopOptionsJs>) -> Result<()> {
    let opts = options.map(Into::into).unwrap_or_default();
    self.inner.tracing().stop(opts).await.into_napi()
  }
}

/// Options bag for `tracing.start`.
#[napi(object)]
pub struct TracingStartOptionsJs {
  pub name: Option<String>,
  pub title: Option<String>,
  pub screenshots: Option<bool>,
  pub snapshots: Option<bool>,
  pub sources: Option<bool>,
}

impl From<TracingStartOptionsJs> for ferridriver::trace::TracingStartOptions {
  fn from(o: TracingStartOptionsJs) -> Self {
    Self {
      name: o.name,
      title: o.title,
      screenshots: o.screenshots.unwrap_or(false),
      snapshots: o.snapshots.unwrap_or(false),
      sources: o.sources.unwrap_or(false),
    }
  }
}

/// Options bag for `tracing.stop` / `tracing.stopChunk`.
#[napi(object)]
pub struct TracingStopOptionsJs {
  pub path: Option<String>,
}

impl From<TracingStopOptionsJs> for ferridriver::trace::TracingStopOptions {
  fn from(o: TracingStopOptionsJs) -> Self {
    Self {
      path: o.path.map(std::path::PathBuf::from),
    }
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
