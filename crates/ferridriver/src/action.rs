//! Deferred action builders: every option-taking operation returns an
//! [`Action`] that runs when awaited, so the default path needs no option
//! arguments and options chain as setters:
//!
//! ```ignore
//! page.goto("https://example.com").await?;
//! page.goto(url).wait_until("networkidle").timeout(10_000).await?;
//! locator.click().button(MouseButton::Right).position((10.0, 20.0)).await?;
//! ```
//!
//! Bindings that already hold a parsed option bag lower it wholesale via
//! [`Action::options`] / [`Action::maybe_options`].

use std::future::{Future, IntoFuture};
use std::pin::Pin;

use crate::error::Result;

/// Boxed future produced by an [`Action`].
pub type ActionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A deferred operation with option bag `O`, resolving to `T` when awaited.
#[must_use = "actions do nothing until awaited"]
pub struct Action<'a, O, T> {
  opts: O,
  run: Box<dyn FnOnce(O) -> ActionFuture<'a, T> + Send + 'a>,
  /// Where the caller wrote this action, for the trace and for
  /// `test --debug`. Every builder is `#[track_caller]`, so the location
  /// captured below is the user's line and not the builder's body.
  origin: crate::trace::CallOrigin,
  signal: Option<crate::abort::AbortSignal>,
}

impl<'a, O: Default, T> Action<'a, O, T> {
  #[track_caller]
  pub(crate) fn new(run: impl FnOnce(O) -> ActionFuture<'a, T> + Send + 'a) -> Self {
    Self {
      opts: O::default(),
      run: Box::new(run),
      origin: crate::trace::call_origin_here(),
      signal: None,
    }
  }
}

impl<O, T> Action<'_, O, T> {
  /// Replace the whole option bag.
  pub fn options(mut self, opts: O) -> Self {
    self.opts = opts;
    self
  }

  /// Replace the option bag when `Some`; keep defaults when `None`.
  pub fn maybe_options(self, opts: Option<O>) -> Self {
    match opts {
      Some(o) => self.options(o),
      None => self,
    }
  }

  /// Playwright's `signal` option: stop the operation with `AbortError`
  /// when `signal` fires.
  pub fn signal(mut self, signal: crate::abort::AbortSignal) -> Self {
    self.signal = Some(signal);
    self
  }

  /// [`Action::signal`] when `Some`.
  pub fn maybe_signal(mut self, signal: Option<crate::abort::AbortSignal>) -> Self {
    self.signal = signal.or(self.signal);
    self
  }
}

// `T: 'a` because scoping the call origin wraps the boxed future in a new
// type, which the `+ 'a` object bound then has to hold for.
impl<'a, O, T: 'a> IntoFuture for Action<'a, O, T> {
  type Output = Result<T>;
  type IntoFuture = ActionFuture<'a, T>;

  fn into_future(self) -> Self::IntoFuture {
    let origin = self.origin;
    let mut fut = (self.run)(self.opts);
    if let Some(signal) = self.signal {
      fut = Box::pin(crate::abort::scope(Some(signal), crate::abort::guard(fut)));
    } else if crate::abort::current().is_some() {
      fut = Box::pin(crate::abort::guard(fut));
    }
    // Nothing captured a site (nothing is tracing, observing or gating), so
    // there is nothing to scope and no reason to box a second time.
    if origin.location.is_none() {
      return fut;
    }
    Box::pin(crate::trace::with_call_origin(origin, fut))
  }
}

/// Generates one chainable setter on `Action<'_, $opts, T>`.
///
/// - `opt` fields are `Option<U>` on the bag: the setter takes `impl Into<U>`.
/// - `vec` fields are `Vec<U>`: the setter takes any `IntoIterator<Item = U>`.
/// - `raw` fields are assigned directly through `Into`.
macro_rules! action_setter {
  ($opts:path, opt $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      pub fn $name(mut self, value: impl Into<$ty>) -> Self {
        self.opts.$name = Some(value.into());
        self
      }
    }
  };
  ($opts:path, vec $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      pub fn $name(mut self, value: impl IntoIterator<Item = $ty>) -> Self {
        self.opts.$name = value.into_iter().collect();
        self
      }
    }
  };
  ($opts:path, raw $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      pub fn $name(mut self, value: impl Into<$ty>) -> Self {
        self.opts.$name = value.into();
        self
      }
    }
  };
  // Millisecond fields (timeouts, delays): accept `u64` ms or `Duration`.
  ($opts:path, ms $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option (milliseconds or `Duration`).")]
      pub fn $name(mut self, value: impl Into<crate::options::TimeoutMs>) -> Self {
        self.opts.$name = Some(value.into().0);
        self
      }
    }
  };
  // `Vec<String>` fields: accept any iterator of string-likes.
  ($opts:path, strs $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      pub fn $name(mut self, value: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.opts.$name = value.into_iter().map(Into::into).collect();
        self
      }
    }
  };
  // For fields whose name would collide with a Rust method-naming
  // convention (`is_*` setters taking `self` by value trip
  // `clippy::wrong_self_convention`).
  ($opts:path, named $method:ident $name:ident, $ty:ty) => {
    impl<T> Action<'_, $opts, T> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      pub fn $method(mut self, value: impl Into<$ty>) -> Self {
        self.opts.$name = Some(value.into());
        self
      }
    }
  };
}

macro_rules! action_options {
  ($opts:path { $( $kind:ident $($name:ident)+ : $ty:ty ),* $(,)? }) => {
    $( action_setter!($opts, $kind $($name)+, $ty); )*
  };
}

action_options!(crate::options::GotoOptions {
  opt wait_until: crate::options::LoadState,
  ms timeout: u64,
});

action_options!(crate::options::ClickOptions {
  opt button: crate::options::MouseButton,
  opt click_count: u32,
  ms delay: u64,
  opt force: bool,
  vec modifiers: crate::options::Modifier,
  opt no_wait_after: bool,
  opt position: crate::options::Point,
  opt steps: u32,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::DblClickOptions {
  opt button: crate::options::MouseButton,
  ms delay: u64,
  opt force: bool,
  vec modifiers: crate::options::Modifier,
  opt no_wait_after: bool,
  opt position: crate::options::Point,
  opt steps: u32,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::FillOptions {
  opt force: bool,
  opt no_wait_after: bool,
  ms timeout: u64,
});

action_options!(crate::options::PressOptions {
  ms delay: u64,
  opt no_wait_after: bool,
  ms timeout: u64,
});

action_options!(crate::options::TypeOptions {
  ms delay: u64,
  opt no_wait_after: bool,
  ms timeout: u64,
});

action_options!(crate::options::CheckOptions {
  opt force: bool,
  opt no_wait_after: bool,
  opt position: crate::options::Point,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::SelectOptionOptions {
  opt force: bool,
  opt no_wait_after: bool,
  ms timeout: u64,
});

action_options!(crate::options::SetInputFilesOptions {
  opt no_wait_after: bool,
  ms timeout: u64,
});

action_options!(crate::options::DispatchEventOptions {
  ms timeout: u64,
});

action_options!(crate::options::HoverOptions {
  opt force: bool,
  vec modifiers: crate::options::Modifier,
  opt no_wait_after: bool,
  opt position: crate::options::Point,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::TapOptions {
  opt force: bool,
  vec modifiers: crate::options::Modifier,
  opt no_wait_after: bool,
  opt position: crate::options::Point,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::DragAndDropOptions {
  opt force: bool,
  opt no_wait_after: bool,
  opt source_position: crate::options::Point,
  opt target_position: crate::options::Point,
  opt steps: u32,
  opt strict: bool,
  ms timeout: u64,
  opt trial: bool,
});

action_options!(crate::options::DropOptions {
  vec modifiers: crate::options::Modifier,
  opt position: crate::options::Point,
  ms timeout: u64,
});

action_options!(crate::options::EvaluateOptions {
  ms timeout: u64,
});

action_options!(crate::options::TimeoutOptions {
  ms timeout: u64,
});

action_options!(crate::options::SelectorReadOptions {
  opt strict: bool,
  ms timeout: u64,
});

action_options!(crate::options::WaitOptions {
  opt state: crate::options::WaitState,
  opt strict: bool,
  ms timeout: u64,
});

action_options!(crate::options::WaitForUrlOptions {
  opt wait_until: crate::options::LoadState,
  ms timeout: u64,
});

action_options!(crate::backend::ClearCookieOptions {
  opt name: crate::options::StringOrRegex,
  opt domain: crate::options::StringOrRegex,
  opt path: crate::options::StringOrRegex,
});

action_options!(crate::coverage::JsCoverageOptions {
  opt reset_on_navigation: bool,
  opt report_anonymous_scripts: bool,
});

action_options!(crate::coverage::CssCoverageOptions {
  opt reset_on_navigation: bool,
});

action_options!(crate::trace::TracingStartOptions {
  opt name: String,
  opt title: String,
  raw screenshots: bool,
  raw snapshots: bool,
  raw sources: bool,
});

action_options!(crate::trace::TracingChunkOptions {
  opt name: String,
  opt title: String,
});

action_options!(crate::trace::TracingStopOptions {
  opt path: std::path::PathBuf,
});

action_options!(crate::accessibility::AccessibilityOptions {
  strs include: String,
  strs exclude: String,
  strs tags: String,
});

action_options!(crate::audits::PageQualityOptions {
  strs only: String,
});

action_options!(crate::options::AddScriptTagOptions {
  opt content: String,
  opt path: std::path::PathBuf,
  opt script_type: String,
  opt url: String,
});

action_options!(crate::options::AddStyleTagOptions {
  opt content: String,
  opt path: std::path::PathBuf,
  opt url: String,
});

action_options!(crate::options::WaitForFunctionOptions {
  opt polling: crate::options::Polling,
  ms timeout: u64,
});

action_options!(crate::options::WaitForLoadStateOptions {
  ms timeout: u64,
});

action_options!(crate::options::AriaSnapshotOptions {
  opt mode: crate::options::AriaSnapshotMode,
  opt depth: i32,
  opt boxes: bool,
  ms timeout: u64,
});

action_options!(crate::snapshot::SnapshotOptions {
  opt depth: i32,
  opt track: String,
});

action_options!(crate::options::ScreenshotOptions {
  opt animations: crate::options::AnimationsMode,
  opt caret: crate::options::CaretMode,
  opt clip: crate::options::ClipRect,
  opt full_page: bool,
  opt format: crate::options::ScreenshotFormat,
  vec mask: crate::locator::Locator,
  opt mask_color: String,
  opt omit_background: bool,
  opt path: std::path::PathBuf,
  opt quality: i64,
  opt scale: crate::options::ScreenshotScale,
  opt style: String,
  ms timeout: u64,
});

action_options!(crate::options::PdfOptions {
  opt format: String,
  opt path: std::path::PathBuf,
  opt scale: f64,
  opt display_header_footer: bool,
  opt header_template: String,
  opt footer_template: String,
  opt print_background: bool,
  opt landscape: bool,
  opt page_ranges: String,
  opt width: crate::options::PdfSize,
  opt height: crate::options::PdfSize,
  opt margin: crate::options::PdfMargin,
  opt prefer_css_page_size: bool,
  opt outline: bool,
  opt tagged: bool,
});

action_options!(crate::options::ElementScreenshotOptions {
  opt animations: crate::options::AnimationsMode,
  opt caret: crate::options::CaretMode,
  opt format: crate::options::ScreenshotFormat,
  vec mask: crate::locator::Locator,
  opt mask_color: String,
  opt omit_background: bool,
  opt path: std::path::PathBuf,
  opt quality: i64,
  opt scale: crate::options::ScreenshotScale,
  opt style: String,
  ms timeout: u64,
});

action_options!(crate::options::ContextCloseOptions {
  opt reason: String,
});

action_options!(crate::page::MouseMoveOptions {
  opt steps: u32,
});

action_options!(crate::options::PageCloseOptions {
  opt run_before_unload: bool,
  opt reason: String,
});

action_options!(crate::options::BrowserCloseOptions {
  opt reason: String,
});

action_options!(crate::page::KeyboardPressOptions {
  ms delay: u64,
});

action_options!(crate::page::KeyboardTypeOptions {
  ms delay: u64,
  opt named_keys: bool,
});

action_options!(crate::page::MouseClickOptions {
  opt button: crate::options::MouseButton,
  opt click_count: u32,
  ms delay: u64,
});

action_options!(crate::page::MouseDownOptions {
  opt button: crate::options::MouseButton,
  opt click_count: u32,
});

action_options!(crate::page::MouseUpOptions {
  opt button: crate::options::MouseButton,
  opt click_count: u32,
});

action_options!(crate::options::EmulateMediaOptions {
  raw media: crate::options::MediaOverride,
  raw color_scheme: crate::options::MediaOverride,
  raw reduced_motion: crate::options::MediaOverride,
  raw forced_colors: crate::options::MediaOverride,
  raw contrast: crate::options::MediaOverride,
});

action_options!(crate::har::RouteFromHarOptions {
  opt url: crate::url_matcher::UrlMatcher,
  raw not_found: crate::har::HarNotFound,
});

action_options!(crate::options::StorageStateOptions {
  opt path: std::path::PathBuf,
  opt indexed_db: bool,
  opt opfs: bool,
  opt credentials: bool,
});

action_options!(crate::options::BrowserContextOptions {
  opt accept_downloads: bool,
  opt base_url: String,
  opt bypass_csp: bool,
  raw color_scheme: crate::options::MediaOverride,
  raw contrast: crate::options::MediaOverride,
  opt device_scale_factor: f64,
  opt extra_http_headers: rustc_hash::FxHashMap<String, String>,
  raw forced_colors: crate::options::MediaOverride,
  opt geolocation: crate::options::Geolocation,
  opt has_touch: bool,
  opt http_credentials: Vec<crate::options::HttpCredentials>,
  opt ignore_https_errors: bool,
  named mobile is_mobile: bool,
  opt java_script_enabled: bool,
  opt locale: String,
  opt offline: bool,
  opt network_conditions: crate::options::NetworkConditions,
  opt cpu_throttling_rate: f64,
  opt permissions: Vec<String>,
  opt proxy: crate::options::ProxyConfig,
  opt record_har: crate::options::RecordHarOptions,
  opt record_video: crate::options::RecordVideoOptions,
  raw reduced_motion: crate::options::MediaOverride,
  opt screen: crate::options::ScreenSize,
  opt service_workers: crate::options::ServiceWorkerPolicy,
  opt storage_state: crate::options::StorageStateInput,
  opt strict_selectors: bool,
  opt timezone_id: String,
  opt user_agent: String,
  raw viewport: crate::options::ViewportOption,
});

/// Playwright's `APIRequestContext` per-request options as setters.
impl<T> Action<'_, crate::http_client::RequestOptions, T> {
  /// Playwright's `method` option.
  pub fn method(mut self, method: impl Into<String>) -> Self {
    self.opts.method = Some(method.into());
    self
  }

  /// Add one header (Playwright's `headers` option, one entry at a time).
  pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self
      .opts
      .headers
      .get_or_insert_with(Vec::new)
      .push((name.into(), value.into()));
    self
  }

  /// Playwright's `headers` option: replaces any headers set so far.
  pub fn headers<K: Into<String>, V: Into<String>>(mut self, headers: impl IntoIterator<Item = (K, V)>) -> Self {
    self.opts.headers = Some(headers.into_iter().map(|(k, v)| (k.into(), v.into())).collect());
    self
  }

  /// Playwright's `data` option as a string or bytes, sent verbatim.
  pub fn data(mut self, body: impl Into<Vec<u8>>) -> Self {
    self.opts.data = Some(body.into());
    self
  }

  /// Playwright's `data` option as a serializable value: sent as JSON
  /// with `content-type: application/json`.
  pub fn json(mut self, body: impl Into<serde_json::Value>) -> Self {
    self.opts.json_data = Some(body.into());
    self
  }

  /// Playwright's `form` option: an `application/x-www-form-urlencoded`
  /// body.
  pub fn form<K: Into<String>, V: Into<String>>(mut self, fields: impl IntoIterator<Item = (K, V)>) -> Self {
    self.opts.form = Some(fields.into_iter().map(|(k, v)| (k.into(), v.into())).collect());
    self
  }

  /// Playwright's `multipart` option: a `multipart/form-data` body.
  pub fn multipart(mut self, fields: impl IntoIterator<Item = crate::http_client::MultipartField>) -> Self {
    self.opts.multipart = Some(fields.into_iter().collect());
    self
  }

  /// Add one query parameter.
  pub fn param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self
      .opts
      .params
      .get_or_insert_with(Vec::new)
      .push((name.into(), value.into()));
    self
  }

  /// Playwright's `params` option: replaces any parameters set so far.
  pub fn params<K: Into<String>, V: Into<String>>(mut self, params: impl IntoIterator<Item = (K, V)>) -> Self {
    self.opts.params = Some(params.into_iter().map(|(k, v)| (k.into(), v.into())).collect());
    self
  }

  /// Playwright's `timeout` option (milliseconds or `Duration`).
  pub fn timeout(mut self, timeout: impl Into<crate::options::TimeoutMs>) -> Self {
    self.opts.timeout = Some(std::time::Duration::from_millis(timeout.into().0));
    self
  }

  /// Playwright's `failOnStatusCode` option.
  pub fn fail_on_status_code(mut self, fail: bool) -> Self {
    self.opts.fail_on_status_code = Some(fail);
    self
  }

  /// Playwright's `maxRedirects` option.
  pub fn max_redirects(mut self, max: u32) -> Self {
    self.opts.max_redirects = Some(max);
    self
  }

  /// Playwright's `maxRetries` option.
  pub fn max_retries(mut self, max: u32) -> Self {
    self.opts.max_retries = Some(max);
    self
  }

  /// Playwright's `ignoreHTTPSErrors` option.
  pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
    self.opts.ignore_https_errors = Some(ignore);
    self
  }
}
