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

/// Playwright `JSCoverageEntry`.
#[napi(object, js_name = "JSCoverageEntry")]
pub struct JsCoverageEntry {
  pub url: String,
  pub script_id: String,
  pub source: Option<String>,
  #[napi(
    ts_type = "Array<{ functionName: string; isBlockCoverage: boolean; ranges: Array<{ count: number; startOffset: number; endOffset: number }> }>"
  )]
  pub functions: Vec<serde_json::Value>,
}

/// One used byte range inside a CSS stylesheet.
#[napi(object)]
pub struct CssCoverageRange {
  pub start: f64,
  pub end: f64,
}

/// Playwright `CSSCoverageEntry`.
#[napi(object, js_name = "CSSCoverageEntry")]
pub struct CssCoverageEntry {
  pub url: String,
  pub text: Option<String>,
  pub ranges: Vec<CssCoverageRange>,
}

impl From<ferridriver::coverage::JsCoverageEntry> for JsCoverageEntry {
  fn from(entry: ferridriver::coverage::JsCoverageEntry) -> Self {
    Self {
      url: entry.url,
      script_id: entry.script_id,
      source: entry.source,
      functions: entry.functions,
    }
  }
}

impl From<ferridriver::coverage::CssCoverageEntry> for CssCoverageEntry {
  fn from(entry: ferridriver::coverage::CssCoverageEntry) -> Self {
    Self {
      url: entry.url,
      text: entry.text,
      ranges: entry
        .ranges
        .into_iter()
        .map(|r| CssCoverageRange {
          start: crate::types::u64_to_f64(r.start),
          end: crate::types::u64_to_f64(r.end),
        })
        .collect(),
    }
  }
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
      .start_js_coverage()
      .options(ferridriver::coverage::JsCoverageOptions {
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
  pub async fn stop_js_coverage(&self) -> Result<Vec<JsCoverageEntry>> {
    let entries = self.inner.stop_js_coverage().await.into_napi()?;
    Ok(entries.into_iter().map(Into::into).collect())
  }

  /// Playwright: `coverage.startCSSCoverage(options?)`.
  #[napi(js_name = "startCSSCoverage")]
  pub async fn start_css_coverage(&self, options: Option<CssCoverageOptions>) -> Result<()> {
    self
      .inner
      .start_css_coverage()
      .options(ferridriver::coverage::CssCoverageOptions {
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
  pub async fn stop_css_coverage(&self) -> Result<Vec<CssCoverageEntry>> {
    let entries = self.inner.stop_css_coverage().await.into_napi()?;
    Ok(entries.into_iter().map(Into::into).collect())
  }
}
