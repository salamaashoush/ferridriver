//! NAPI binding for [`ferridriver::coverage::Coverage`]. Playwright
//! `class Coverage` (`packages/playwright-core/src/client/coverage.ts`).

use napi::Result;
use napi_derive::napi;

use crate::error::IntoNapi;

/// `coverage.startJSCoverage(options?)`'s bag.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct JsCoverageOptions {
  /// Forget scripts on navigation. Defaults to `true`.
  pub reset_on_navigation: Option<bool>,
  /// Report scripts without a URL. Defaults to `false`.
  pub report_anonymous_scripts: Option<bool>,
}

/// `coverage.startCSSCoverage(options?)`'s bag.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct CssCoverageOptions {
  /// Forget stylesheets on navigation. Defaults to `true`.
  pub reset_on_navigation: Option<bool>,
}

/// Playwright's `page.coverage`. Chromium only.
#[napi]
pub struct Coverage {
  inner: ferridriver::coverage::Coverage,
}

impl Coverage {
  pub(crate) fn wrap(inner: ferridriver::coverage::Coverage) -> Self {
    Self { inner }
  }
}

#[napi]
impl Coverage {
  /// Playwright: `coverage.startJSCoverage(options?)`.
  #[napi(js_name = "startJSCoverage")]
  pub async fn start_js_coverage(&self, options: Option<JsCoverageOptions>) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .start_js_coverage(ferridriver::coverage::JsCoverageOptions {
        reset_on_navigation: options.reset_on_navigation,
        report_anonymous_scripts: options.report_anonymous_scripts,
      })
      .await
      .into_napi()
  }

  /// Playwright: `coverage.stopJSCoverage(): Promise<JSCoverageEntry[]>`.
  #[napi(
    js_name = "stopJSCoverage",
    ts_return_type = "Promise<Array<{ url: string; scriptId: string; source?: string; functions: Array<{ functionName: string; isBlockCoverage: boolean; ranges: Array<{ count: number; startOffset: number; endOffset: number }> }> }>>"
  )]
  pub async fn stop_js_coverage(&self) -> Result<serde_json::Value> {
    let entries = self.inner.stop_js_coverage().await.into_napi()?;
    serde_json::to_value(entries).map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `coverage.startCSSCoverage(options?)`.
  #[napi(js_name = "startCSSCoverage")]
  pub async fn start_css_coverage(&self, options: Option<CssCoverageOptions>) -> Result<()> {
    self
      .inner
      .start_css_coverage(ferridriver::coverage::CssCoverageOptions {
        reset_on_navigation: options.and_then(|options| options.reset_on_navigation),
      })
      .await
      .into_napi()
  }

  /// Playwright: `coverage.stopCSSCoverage(): Promise<CSSCoverageEntry[]>`.
  #[napi(
    js_name = "stopCSSCoverage",
    ts_return_type = "Promise<Array<{ url: string; text?: string; ranges: Array<{ start: number; end: number }> }>>"
  )]
  pub async fn stop_css_coverage(&self) -> Result<serde_json::Value> {
    let entries = self.inner.stop_css_coverage().await.into_napi()?;
    serde_json::to_value(entries).map_err(|e| napi::Error::from_reason(e.to_string()))
  }
}
