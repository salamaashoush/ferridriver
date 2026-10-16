//! `CoverageJs`: Playwright's `page.coverage` (`client/coverage.ts`).
//! Chromium only.

use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::{FerriResultCtxExt, serde_from_js, serde_to_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Coverage")]
pub struct CoverageJs {
  #[qjs(skip_trace)]
  inner: ferridriver::coverage::Coverage,
}

impl CoverageJs {
  #[must_use]
  pub(crate) fn new(inner: ferridriver::coverage::Coverage) -> Self {
    Self { inner }
  }
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsCoverageOptions {
  reset_on_navigation: Option<bool>,
  report_anonymous_scripts: Option<bool>,
}

fn options<'js>(ctx: &Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<JsCoverageOptions> {
  match options.0 {
    Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value),
    _ => Ok(JsCoverageOptions::default()),
  }
}

#[rquickjs::methods]
impl CoverageJs {
  /// Playwright: `coverage.startJSCoverage(options?)`.
  #[qjs(rename = "startJSCoverage")]
  pub async fn start_js_coverage<'js>(&self, ctx: Ctx<'js>, options_bag: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let options = options(&ctx, options_bag)?;
    self
      .inner
      .start_js_coverage(ferridriver::coverage::JsCoverageOptions {
        reset_on_navigation: options.reset_on_navigation,
        report_anonymous_scripts: options.report_anonymous_scripts,
      })
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `coverage.stopJSCoverage(): Promise<JSCoverageEntry[]>`.
  #[qjs(rename = "stopJSCoverage")]
  pub async fn stop_js_coverage<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let entries = self.inner.stop_js_coverage().await.into_js_with(&ctx)?;
    serde_to_js(&ctx, &entries)
  }

  /// Playwright: `coverage.startCSSCoverage(options?)`.
  #[qjs(rename = "startCSSCoverage")]
  pub async fn start_css_coverage<'js>(&self, ctx: Ctx<'js>, options_bag: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let options = options(&ctx, options_bag)?;
    self
      .inner
      .start_css_coverage(ferridriver::coverage::CssCoverageOptions {
        reset_on_navigation: options.reset_on_navigation,
      })
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `coverage.stopCSSCoverage(): Promise<CSSCoverageEntry[]>`.
  #[qjs(rename = "stopCSSCoverage")]
  pub async fn stop_css_coverage<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let entries = self.inner.stop_css_coverage().await.into_js_with(&ctx)?;
    serde_to_js(&ctx, &entries)
  }
}
