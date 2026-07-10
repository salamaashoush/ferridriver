//! Option-bag parsers for the `page` binding surface (shared with
//! `context` / `locator` / `frame` where noted).

use ferridriver::options::WaitOptions;
use rquickjs::function::Opt;
use serde::Deserialize;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::serde_from_js;
use crate::bindings::locator::LocatorJs;

pub(crate) fn parse_wait_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<WaitOptions> {
  Ok(crate::bindings::convert::parse_opt_bag(ctx, value)?.unwrap_or_default())
}

pub(crate) fn parse_goto_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::GotoOptions>> {
  crate::bindings::convert::parse_opt_bag(ctx, value)
}

/// Parse the Playwright-shaped `emulateMedia` options bag from a
/// `rquickjs::Value`. Unlike `serde_from_js`, this walks the JS object
/// manually so we can distinguish three states for every field:
///
/// * absent → [`MediaOverride::Unchanged`]
/// * explicit `null` → [`MediaOverride::Disabled`]
/// * string value → [`MediaOverride::Set`]
///
/// serde-based deserialization conflates `undefined` and `null` into a
/// single `Option::None`, which breaks the Playwright null-disables-the-
/// override contract. See `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`
/// for the `T | null | undefined` shape we're mirroring.
pub(crate) fn parse_emulate_media_field<'js>(
  obj: &rquickjs::Object<'js>,
  key: &str,
) -> rquickjs::Result<ferridriver::options::MediaOverride> {
  use ferridriver::options::MediaOverride;
  if !obj.contains_key(key)? {
    return Ok(MediaOverride::Unchanged);
  }
  let val: rquickjs::Value<'js> = obj.get(key)?;
  if val.is_undefined() {
    Ok(MediaOverride::Unchanged)
  } else if val.is_null() {
    Ok(MediaOverride::Disabled)
  } else if let Some(s) = val.as_string() {
    Ok(MediaOverride::Set(s.to_string()?))
  } else {
    Err(rquickjs::Error::new_from_js_message(
      "emulateMedia options",
      "field",
      format!("{key}: expected null, undefined, or string"),
    ))
  }
}

pub(crate) fn parse_emulate_media_options<'js>(
  _ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::EmulateMediaOptions> {
  let Some(v) = value.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
    return Ok(ferridriver::options::EmulateMediaOptions::default());
  };
  let Some(obj) = v.as_object() else {
    return Ok(ferridriver::options::EmulateMediaOptions::default());
  };
  Ok(ferridriver::options::EmulateMediaOptions {
    media: parse_emulate_media_field(obj, "media")?,
    color_scheme: parse_emulate_media_field(obj, "colorScheme")?,
    reduced_motion: parse_emulate_media_field(obj, "reducedMotion")?,
    forced_colors: parse_emulate_media_field(obj, "forcedColors")?,
    contrast: parse_emulate_media_field(obj, "contrast")?,
  })
}

pub(crate) fn parse_drag_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DragAndDropOptions>> {
  crate::bindings::convert::parse_opt_bag(ctx, value)
}

pub(crate) fn parse_page_close_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::PageCloseOptions>> {
  crate::bindings::convert::parse_opt_bag(ctx, value)
}

/// Read an optional string field from an options bag (absent / non-object
/// / non-string → `None`). Shared by `addScriptTag` / `addStyleTag`.
pub(crate) fn opt_str_field(options: &Opt<rquickjs::Value<'_>>, key: &str) -> rquickjs::Result<Option<String>> {
  let Some(v) = options.0.as_ref() else { return Ok(None) };
  let Some(obj) = v.as_object() else { return Ok(None) };
  let field: rquickjs::Value<'_> = obj.get(key)?;
  match field.as_string() {
    Some(s) => Ok(Some(s.to_string()?)),
    None => Ok(None),
  }
}

/// Read an optional `{ timeout }` (ms) field from an options bag, clamped
/// to `>= 0`. Shared by `waitForNavigation`.
pub(crate) fn opt_timeout_ms(options: &Opt<rquickjs::Value<'_>>) -> rquickjs::Result<Option<u64>> {
  let Some(v) = options.0.as_ref() else { return Ok(None) };
  let Some(obj) = v.as_object() else { return Ok(None) };
  let field: rquickjs::Value<'_> = obj.get("timeout")?;
  #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
  Ok(field.as_number().map(|n| if n < 0.0 { 0 } else { n as u64 }))
}

pub(crate) fn parse_unroute_behavior(behavior: &str) -> rquickjs::Result<ferridriver::options::UnrouteBehavior> {
  match behavior {
    "default" => Ok(ferridriver::options::UnrouteBehavior::Default),
    "wait" => Ok(ferridriver::options::UnrouteBehavior::Wait),
    "ignoreErrors" => Ok(ferridriver::options::UnrouteBehavior::IgnoreErrors),
    other => Err(rquickjs::Error::new_from_js_message(
      "unrouteAll options",
      "behavior",
      format!("invalid behavior {other:?} (expected 'wait', 'ignoreErrors', or 'default')"),
    )),
  }
}

/// Extract the `times` field from a `route(url, handler, { times })` options
/// bag. Absent/undefined options or a missing `times` yields `None`
/// (unlimited). Shared by `page.route` and `context.route`.
pub(crate) fn parse_route_times(
  options: &rquickjs::function::Opt<rquickjs::Value<'_>>,
) -> rquickjs::Result<Option<u32>> {
  let Some(v) = options.0.as_ref() else { return Ok(None) };
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  let Some(obj) = v.as_object() else { return Ok(None) };
  let t: rquickjs::Value<'_> = obj.get("times")?;
  if t.is_undefined() || t.is_null() {
    return Ok(None);
  }
  #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
  Ok(t.as_number().map(|n| if n < 0.0 { 0 } else { n as u32 }))
}

/// Parse the `{ url?, notFound?, update?, updateContent?, updateMode? }`
/// options bag for `routeFromHAR`. Shared by `page.routeFromHAR` and
/// `context.routeFromHAR`. `url` accepts a glob string or `RegExp`.
pub(crate) fn parse_har_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  options: &rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::har::RouteFromHarOptions> {
  let mut out = ferridriver::har::RouteFromHarOptions::default();
  let Some(v) = options.0.as_ref() else { return Ok(out) };
  let Some(obj) = v.as_object() else { return Ok(out) };
  let url: rquickjs::Value<'js> = obj.get("url")?;
  if !url.is_undefined() && !url.is_null() {
    out.url = Some(url_value_to_matcher(ctx, url)?);
  }
  let nf: rquickjs::Value<'_> = obj.get("notFound")?;
  if let Some(s) = nf.as_string() {
    match s.to_string()?.as_str() {
      "fallback" => out.not_found = ferridriver::har::HarNotFound::Fallback,
      "abort" => out.not_found = ferridriver::har::HarNotFound::Abort,
      other => {
        return Err(rquickjs::Error::new_from_js_message(
          "routeFromHAR",
          "notFound",
          format!("invalid notFound {other:?} (expected 'abort' or 'fallback')"),
        ));
      },
    }
  }
  out.update = obj.get::<_, Option<bool>>("update")?.unwrap_or(false);
  out.update_content = match obj.get::<_, Option<String>>("updateContent")?.as_deref() {
    Some("attach") => Some(ferridriver::tracing::HarContentPolicy::Attach),
    Some("embed") => Some(ferridriver::tracing::HarContentPolicy::Embed),
    None => None,
    Some(other) => {
      return Err(rquickjs::Error::new_from_js_message(
        "routeFromHAR",
        "updateContent",
        format!("invalid updateContent {other:?} (expected 'attach' or 'embed')"),
      ));
    },
  };
  out.update_mode = match obj.get::<_, Option<String>>("updateMode")?.as_deref() {
    Some("minimal") => Some(ferridriver::tracing::HarMode::Minimal),
    Some("full") => Some(ferridriver::tracing::HarMode::Full),
    None => None,
    Some(other) => {
      return Err(rquickjs::Error::new_from_js_message(
        "routeFromHAR",
        "updateMode",
        format!("invalid updateMode {other:?} (expected 'minimal' or 'full')"),
      ));
    },
  };
  Ok(out)
}

/// Shape of `page.screenshot` options accepted from JS. Full Playwright
/// `PageScreenshotOptions` surface per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsScreenshotOptions {
  animations: Option<String>,
  caret: Option<String>,
  clip: Option<JsClipRect>,
  full_page: Option<bool>,
  #[serde(rename = "type")]
  format: Option<String>,
  // `mask` is NOT decoded here: Playwright takes `Locator[]`, and a
  // `LocatorJs` class instance is not serde-deserialisable. It is read
  // manually from the options object via `parse_mask_locators` before
  // this struct is built.
  #[serde(skip)]
  _mask_placeholder: (),
  mask_color: Option<String>,
  omit_background: Option<bool>,
  path: Option<String>,
  quality: Option<i64>,
  scale: Option<String>,
  style: Option<String>,
  timeout: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Clone, Copy)]
struct JsClipRect {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

impl From<JsClipRect> for ferridriver::options::ClipRect {
  fn from(c: JsClipRect) -> Self {
    Self {
      x: c.x,
      y: c.y,
      width: c.width,
      height: c.height,
    }
  }
}

/// Read `mask: Locator[]` from the screenshot options object. Each entry
/// must be a `LocatorJs` class instance (Playwright's `mask?: Locator[]`);
/// the core `Locator` is cloned out so the selector string is extracted
/// Rust-side before backend dispatch.
pub(crate) fn parse_mask_locators<'js>(obj: &rquickjs::Object<'js>) -> rquickjs::Result<Vec<ferridriver::Locator>> {
  let v: rquickjs::Value<'js> = obj.get("mask")?;
  if v.is_undefined() || v.is_null() {
    return Ok(Vec::new());
  }
  let arr = v.into_array().ok_or_else(|| {
    rquickjs::Error::new_from_js_message("screenshot options", "mask", "expected an array of Locator")
  })?;
  let mut out = Vec::with_capacity(arr.len());
  for item in arr.iter::<rquickjs::Value<'js>>() {
    let item = item?;
    if let Ok(class) = rquickjs::Class::<LocatorJs>::from_value(&item) {
      out.push(class.borrow().inner_ref().clone());
    } else {
      return Err(rquickjs::Error::new_from_js_message(
        "screenshot options",
        "mask",
        "each mask entry must be a Locator instance",
      ));
    }
  }
  Ok(out)
}

pub(crate) fn parse_screenshot_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::ScreenshotOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let mask = match v.as_object() {
        Some(obj) => parse_mask_locators(obj)?,
        None => Vec::new(),
      };
      let js: JsScreenshotOptions = serde_from_js(ctx, v)?;
      Ok(ferridriver::options::ScreenshotOptions {
        animations: js.animations.as_deref().map(ferridriver::options::AnimationsMode::from),
        caret: js.caret.as_deref().map(ferridriver::options::CaretMode::from),
        clip: js.clip.map(Into::into),
        full_page: js.full_page,
        format: js.format.as_deref().map(ferridriver::options::ScreenshotFormat::from),
        mask,
        mask_color: js.mask_color,
        omit_background: js.omit_background,
        path: js.path.map(std::path::PathBuf::from),
        quality: js.quality,
        scale: js.scale.as_deref().map(ferridriver::options::ScreenshotScale::from),
        style: js.style,
        timeout: js.timeout,
      })
    },
    _ => Ok(ferridriver::options::ScreenshotOptions::default()),
  }
}

/// Subset of Playwright's `PDFOptions` exposed to scripts. Path fields and
/// advanced page-range/margin controls are not wired yet; users who need
/// those can use `page.evaluate` with `window.print` or extend here.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsPdfOptions {
  format: Option<String>,
  landscape: Option<bool>,
  print_background: Option<bool>,
  scale: Option<f64>,
  display_header_footer: Option<bool>,
  header_template: Option<String>,
  footer_template: Option<String>,
  page_ranges: Option<String>,
  prefer_css_page_size: Option<bool>,
  outline: Option<bool>,
  tagged: Option<bool>,
}

pub(crate) fn parse_pdf_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::PdfOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsPdfOptions = serde_from_js(ctx, v)?;
      Ok(ferridriver::options::PdfOptions {
        format: js.format,
        path: None,
        scale: js.scale,
        display_header_footer: js.display_header_footer,
        header_template: js.header_template,
        footer_template: js.footer_template,
        print_background: js.print_background,
        landscape: js.landscape,
        page_ranges: js.page_ranges,
        width: None,
        height: None,
        margin: None,
        prefer_css_page_size: js.prefer_css_page_size,
        outline: js.outline,
        tagged: js.tagged,
      })
    },
    _ => Ok(ferridriver::options::PdfOptions::default()),
  }
}

/// Lower a JS `string | RegExp` value into a [`UrlMatcher`]. Mirrors
/// the NAPI `JsRegExpLike` shape — the JS RegExp's `source` and
/// `flags` getters drive `UrlMatcher::regex_from_source`. Plain
/// strings go through `UrlMatcher::glob`.
pub(crate) fn url_value_to_matcher<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: rquickjs::Value<'js>,
) -> rquickjs::Result<ferridriver::url_matcher::UrlMatcher> {
  if let Some(s) = value.as_string() {
    let glob = s.to_string()?;
    return ferridriver::url_matcher::UrlMatcher::glob(glob).into_js_with(ctx);
  }
  if let Some(obj) = value.as_object() {
    // RegExp constructor.name === "RegExp" — also has `source` (string)
    // and `flags` (string) getters per ECMAScript spec.
    let source: rquickjs::Result<String> = obj.get("source");
    let flags: rquickjs::Result<String> = obj.get("flags");
    if let (Ok(source), Ok(flags)) = (source, flags) {
      return ferridriver::url_matcher::UrlMatcher::regex_from_source(&source, &flags).into_js_with(ctx);
    }
  }
  let _ = ctx;
  Err(rquickjs::Error::new_from_js_message(
    "Page.waitFor*",
    "url",
    "expected string | RegExp".to_string(),
  ))
}

/// Lower a JS `string | RegExp` value into a Rust
/// [`ferridriver::options::StringOrRegex`] for every `getBy*` matcher
/// and `RoleOptions.name`. Reads `source` / `flags` via the RegExp
/// prototype getters (same technique as NAPI's `JsRegExpLike`), so a
/// real JS `RegExp` round-trips without a wire-shape escape.
pub(crate) fn string_or_regex_from_js(
  value: rquickjs::Value<'_>,
) -> rquickjs::Result<ferridriver::options::StringOrRegex> {
  if let Some(s) = value.as_string() {
    return Ok(ferridriver::options::StringOrRegex::String(s.to_string()?));
  }
  if let Some(obj) = value.as_object() {
    let source: rquickjs::Result<String> = obj.get("source");
    let flags: rquickjs::Result<String> = obj.get("flags");
    if let (Ok(source), Ok(flags)) = (source, flags) {
      return Ok(ferridriver::options::StringOrRegex::Regex { source, flags });
    }
  }
  Err(rquickjs::Error::new_from_js_message(
    "getBy*",
    "text",
    "expected string | RegExp".to_string(),
  ))
}

/// Parse `{ exact?: boolean }` options for `getByText` / `getByLabel` / etc.
pub(crate) fn parse_text_options(
  value: rquickjs::function::Opt<rquickjs::Value<'_>>,
) -> ferridriver::options::TextOptions {
  let Some(v) = value.0 else {
    return ferridriver::options::TextOptions::default();
  };
  if v.is_undefined() || v.is_null() {
    return ferridriver::options::TextOptions::default();
  }
  let Some(obj) = v.as_object() else {
    return ferridriver::options::TextOptions::default();
  };
  let exact: Option<bool> = obj.get("exact").ok();
  ferridriver::options::TextOptions { exact }
}

/// Parse the `getByRole` options bag. `{ name?: string | RegExp,
/// exact?, checked?, disabled?, expanded?, level?, pressed?,
/// selected?, includeHidden? }`. Mirrors Playwright's `ByRoleOptions`.
pub(crate) fn parse_role_options<'js>(
  value: rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::RoleOptions> {
  let Some(v) = value.0 else {
    return Ok(ferridriver::options::RoleOptions::default());
  };
  if v.is_undefined() || v.is_null() {
    return Ok(ferridriver::options::RoleOptions::default());
  }
  let Some(obj) = v.as_object() else {
    return Ok(ferridriver::options::RoleOptions::default());
  };
  let name_val: Option<rquickjs::Value<'js>> = obj.get("name").ok();
  let name = match name_val {
    Some(val) if !val.is_undefined() && !val.is_null() => Some(string_or_regex_from_js(val)?),
    _ => None,
  };
  let description_val: Option<rquickjs::Value<'js>> = obj.get("description").ok();
  let description = match description_val {
    Some(val) if !val.is_undefined() && !val.is_null() => Some(string_or_regex_from_js(val)?),
    _ => None,
  };
  let exact: Option<bool> = obj.get("exact").ok();
  let checked: Option<bool> = obj.get("checked").ok();
  let disabled: Option<bool> = obj.get("disabled").ok();
  let expanded: Option<bool> = obj.get("expanded").ok();
  let level: Option<i32> = obj.get("level").ok();
  let pressed: Option<bool> = obj.get("pressed").ok();
  let selected: Option<bool> = obj.get("selected").ok();
  let include_hidden: Option<bool> = obj.get("includeHidden").ok();
  Ok(ferridriver::options::RoleOptions {
    name,
    description,
    exact,
    checked,
    disabled,
    expanded,
    level,
    pressed,
    selected,
    include_hidden,
  })
}
