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
}

impl<'a, O: Default, T> Action<'a, O, T> {
  pub(crate) fn new(run: impl FnOnce(O) -> ActionFuture<'a, T> + Send + 'a) -> Self {
    Self {
      opts: O::default(),
      run: Box::new(run),
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
}

impl<'a, O, T> IntoFuture for Action<'a, O, T> {
  type Output = Result<T>;
  type IntoFuture = ActionFuture<'a, T>;

  fn into_future(self) -> Self::IntoFuture {
    (self.run)(self.opts)
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

action_options!(crate::options::WaitOptions {
  opt state: crate::options::WaitState,
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
  opt format: crate::options::ScreenshotFormat,
  opt path: std::path::PathBuf,
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
  opt http_credentials: crate::options::HttpCredentials,
  opt ignore_https_errors: bool,
  named mobile is_mobile: bool,
  opt java_script_enabled: bool,
  opt locale: String,
  opt offline: bool,
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
