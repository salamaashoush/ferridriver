//! Option bags, storage state types, and conversion helpers for `BrowserContext`.

use crate::types::CookieData;
use napi::Result;
use napi_derive::napi;

/// NAPI shape for Playwright's
/// `recordVideo?: { dir: string, size?: { width, height } }` option —
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:10150`.
#[napi(object)]
pub struct RecordVideoOptionsJs {
  pub dir: String,
  pub size: Option<VideoSizeJs>,
}

/// NAPI shape for Playwright's `recordHar` context option. The archive
/// is written when the context closes.
#[napi(object)]
pub struct RecordHarOptionsJs {
  /// Where to write the archive. A `.zip` path packs bodies as separate
  /// entries; anything else writes JSON with bodies inlined.
  pub path: String,
  /// `omit` | `embed` | `attach`. Defaults by path extension.
  pub content: Option<String>,
  /// `full` | `minimal`. Defaults to `full`.
  pub mode: Option<String>,
  /// Deprecated alias for `content: 'omit'`.
  pub omit_content: Option<bool>,
  /// Only record requests whose URL matches. Glob string or `RegExp`.
  #[napi(ts_type = "string | RegExp")]
  pub url_filter: Option<napi::Either<String, crate::types::JsRegExpLike>>,
}

/// NAPI shape for Playwright's `recordVideo.size: { width, height }`.
#[napi(object)]
pub struct VideoSizeJs {
  pub width: f64,
  pub height: f64,
}

/// NAPI shape for Playwright's
/// `BrowserContextOptions` —
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
/// Every field is optional. Fields that must mirror Playwright's
/// string unions use string passthrough here with the exact union
/// rendered via `#[napi(ts_args_type = ...)]` on the consuming
/// `browser.newContext(options)` method. The media fields
/// (`colorScheme`, `contrast`, `forcedColors`, `reducedMotion`) take
/// `null` too, which is not the same as leaving them out: `null` turns
/// the emulation off.
#[napi(object)]
pub struct NapiBrowserContextOptions {
  pub accept_downloads: Option<bool>,
  // napi's camelCase mangles these acronyms (`baseUrl`, `bypassCsp`,
  // `extraHttpHeaders`, `ignoreHttpsErrors`) while Playwright — and the
  // `ts_args_type` on `browser.newContext` — spell them upper-case. Left
  // to infer, the declared option and the bound field disagree and the
  // value is silently dropped.
  #[napi(js_name = "baseURL")]
  pub base_url: Option<String>,
  #[napi(js_name = "bypassCSP")]
  pub bypass_csp: Option<bool>,
  #[napi(ts_type = "null | 'light' | 'dark' | 'no-preference'")]
  pub color_scheme: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'no-preference' | 'more'")]
  pub contrast: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  pub device_scale_factor: Option<f64>,
  #[napi(js_name = "extraHTTPHeaders")]
  pub extra_http_headers: Option<crate::types::HashMap<String, String>>,
  #[napi(ts_type = "null | 'active' | 'none'")]
  pub forced_colors: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  pub geolocation: Option<NapiGeolocation>,
  pub has_touch: Option<bool>,
  #[napi(
    ts_type = "{ username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' } | Array<{ username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' }>"
  )]
  pub http_credentials: Option<NapiHttpCredentialsInput>,
  #[napi(js_name = "ignoreHTTPSErrors")]
  pub ignore_https_errors: Option<bool>,
  pub is_mobile: Option<bool>,
  pub java_script_enabled: Option<bool>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  #[napi(
    ts_type = "'slow-3g' | 'slow-4g' | 'fast-4g' | { latency?: number; downloadThroughput?: number; uploadThroughput?: number }"
  )]
  pub network_conditions: Option<serde_json::Value>,
  pub cpu_throttling: Option<f64>,
  pub permissions: Option<Vec<String>>,
  pub proxy: Option<NapiProxyConfig>,
  pub record_har: Option<RecordHarOptionsJs>,
  pub record_video: Option<RecordVideoOptionsJs>,
  #[napi(ts_type = "null | 'reduce' | 'no-preference'")]
  pub reduced_motion: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  pub screen: Option<NapiScreenSize>,
  pub service_workers: Option<String>,
  /// Playwright: `storageState?: string | StorageState`. Either a path to
  /// a JSON file written by `context.storageState({ path })`, or an inline
  /// state object of the same shape `storageState()` returns. Cookies and
  /// localStorage hydrate before the first navigation.
  #[napi(ts_type = "string | NapiStorageState")]
  pub storage_state: Option<serde_json::Value>,
  pub strict_selectors: Option<bool>,
  pub timezone_id: Option<String>,
  pub user_agent: Option<String>,
  /// Playwright allows `viewport: null` to opt out of viewport
  /// emulation. NAPI inbound deserialisation treats `null` and
  /// `undefined` identically, so we expose an explicit boolean
  /// `disable_viewport` for the `null` case alongside `viewport` for
  /// a concrete size. Callers pass `{ width, height }` to set, or
  /// `{ disableViewport: true }` to opt out. Absent fields =
  /// `undefined` = "browser default".
  pub viewport: Option<NapiViewportSize>,
  pub disable_viewport: Option<bool>,
}

#[napi(object)]
pub struct NapiGeolocation {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NapiHttpCredentials {
  pub username: String,
  pub password: String,
  pub origin: Option<String>,
  pub send: Option<String>,
}

/// Playwright `HttpCredentials | HttpCredentials[]`. The list comes first:
/// an array is an object too, and would be read as one entry.
pub(crate) type NapiHttpCredentialsInput = napi::Either<Vec<NapiHttpCredentials>, NapiHttpCredentials>;

/// The list core takes, or `None` for an empty one.
pub(crate) fn http_credentials_list(
  input: NapiHttpCredentialsInput,
) -> Option<Vec<ferridriver::options::HttpCredentials>> {
  let list: Vec<_> = match input {
    napi::Either::A(entries) => entries.into_iter().map(Into::into).collect(),
    napi::Either::B(entry) => vec![entry.into()],
  };
  (!list.is_empty()).then_some(list)
}

impl From<NapiHttpCredentials> for ferridriver::options::HttpCredentials {
  fn from(c: NapiHttpCredentials) -> Self {
    use ferridriver::options as fo;
    fo::HttpCredentials {
      username: c.username,
      password: c.password,
      origin: c.origin,
      send: c.send.and_then(|s| match s.as_str() {
        "always" => Some(fo::HttpCredentialsSend::Always),
        "unauthorized" => Some(fo::HttpCredentialsSend::Unauthorized),
        _ => None,
      }),
    }
  }
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct NapiProxyConfig {
  pub server: String,
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

#[napi(object)]
pub struct NapiScreenSize {
  pub width: f64,
  pub height: f64,
}

#[napi(object)]
pub struct NapiViewportSize {
  pub width: f64,
  pub height: f64,
}

/// NAPI shape for Playwright's `storageState(options?)`:
/// `{ path?: string, indexedDB?: boolean, opfs?: boolean, credentials?: boolean }`
/// (`packages/playwright-core/src/client/browserContext.ts`).
#[napi(object)]
pub struct NapiStorageStateOptions {
  pub path: Option<String>,
  // napi would spell this `indexedDb`, a key no caller writes.
  #[napi(js_name = "indexedDB")]
  pub indexed_db: Option<bool>,
  pub opfs: Option<bool>,
  pub credentials: Option<bool>,
}

/// A single `localStorage` entry — Playwright `NameValue`.
#[napi(object)]
pub struct NapiNameValue {
  pub name: String,
  pub value: String,
}

/// Per-origin storage snapshot — Playwright `OriginStorage`.
#[napi(object)]
pub struct NapiOriginState {
  pub origin: String,
  pub local_storage: Vec<NapiNameValue>,
  /// Present when `storageState` was called with `indexedDB: true`.
  #[napi(
    js_name = "indexedDB",
    ts_type = "Array<{ name: string; version: number; stores: Array<{ name: string; autoIncrement: boolean; keyPath?: string; keyPathArray?: string[]; records: Array<{ key?: any; keyEncoded?: any; value?: any; valueEncoded?: any }>; indexes: Array<{ name: string; keyPath?: string; keyPathArray?: string[]; multiEntry: boolean; unique: boolean }> }> }>"
  )]
  pub indexed_db: Option<serde_json::Value>,
  /// Present when `storageState` was called with `opfs: true`.
  #[napi(ts_type = "Array<{ path: string; type: 'file' | 'directory'; base64?: string }>")]
  pub opfs: Option<serde_json::Value>,
}

/// Result of `context.storageState()` — Playwright `StorageState`.
#[napi(object)]
pub struct NapiStorageState {
  pub cookies: Vec<CookieData>,
  pub origins: Vec<NapiOriginState>,
  pub credentials: Option<Vec<crate::credentials::VirtualCredential>>,
}

impl TryFrom<ferridriver::options::StorageState> for NapiStorageState {
  type Error = napi::Error;

  fn try_from(s: ferridriver::options::StorageState) -> Result<Self> {
    let origins = s
      .origins
      .into_iter()
      .map(|o| {
        Ok(NapiOriginState {
          indexed_db: o
            .indexed_db
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| storage_json_error(&e))?,
          opfs: o
            .opfs
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| storage_json_error(&e))?,
          origin: o.origin,
          local_storage: o
            .local_storage
            .into_iter()
            .map(|nv| NapiNameValue {
              name: nv.name,
              value: nv.value,
            })
            .collect(),
        })
      })
      .collect::<Result<Vec<_>>>()?;
    Ok(Self {
      cookies: s.cookies.iter().map(CookieData::from).collect(),
      origins,
      credentials: s.credentials.map(|list| list.into_iter().map(Into::into).collect()),
    })
  }
}

fn storage_json_error(error: &serde_json::Error) -> napi::Error {
  napi::Error::from_reason(format!("storageState: {error}"))
}

impl RecordHarOptionsJs {
  /// Lower into the core [`ferridriver::options::RecordHarOptions`]. Unknown
  /// `content` / `mode` values and an invalid `urlFilter` fall back to
  /// `None`, like the context options around it.
  fn into_core(self) -> ferridriver::options::RecordHarOptions {
    use ferridriver::options as fo;
    fo::RecordHarOptions {
      path: std::path::PathBuf::from(self.path),
      content: match self.content.as_deref() {
        Some("omit") => Some(fo::RecordHarContent::Omit),
        Some("embed") => Some(fo::RecordHarContent::Embed),
        Some("attach") => Some(fo::RecordHarContent::Attach),
        _ => None,
      },
      mode: match self.mode.as_deref() {
        Some("minimal") => Some(fo::RecordHarMode::Minimal),
        Some("full") => Some(fo::RecordHarMode::Full),
        _ => None,
      },
      omit_content: self.omit_content,
      url_filter: self.url_filter.and_then(|f| match f {
        napi::Either::A(glob) => ferridriver::url_matcher::UrlMatcher::glob(&glob).ok(),
        napi::Either::B(re) => {
          ferridriver::url_matcher::UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or("")).ok()
        },
      }),
    }
  }
}

impl NapiBrowserContextOptions {
  /// Lower into the core [`ferridriver::options::BrowserContextOptions`]
  /// bag. Unknown string values for enum-typed fields fall back to
  /// `None` (same-as-absent), matching Playwright's lenient client-side
  /// parsing.
  ///
  /// # Errors
  ///
  /// `networkConditions` that is neither a known preset nor valid numbers:
  /// a monitor that silently runs unthrottled measures the wrong thing.
  pub fn into_core(self) -> Result<ferridriver::options::BrowserContextOptions> {
    use ferridriver::options as fo;
    let network_conditions = self
      .network_conditions
      .map(serde_json::from_value::<fo::NetworkConditions>)
      .transpose()
      .map_err(|e| napi::Error::from_reason(format!("networkConditions: {e}")))?;
    let color_scheme = crate::types::lower_override(self.color_scheme);
    let contrast = crate::types::lower_override(self.contrast);
    let forced_colors = crate::types::lower_override(self.forced_colors);
    let reduced_motion = crate::types::lower_override(self.reduced_motion);
    let viewport = if self.disable_viewport == Some(true) {
      fo::ViewportOption::Null
    } else if let Some(vp) = self.viewport {
      fo::ViewportOption::Size {
        width: crate::types::f64_to_i64(vp.width),
        height: crate::types::f64_to_i64(vp.height),
      }
    } else {
      fo::ViewportOption::Default
    };
    let http_credentials = self.http_credentials.and_then(http_credentials_list);
    let proxy = self.proxy.map(|p| fo::ProxyConfig {
      server: p.server,
      bypass: p.bypass,
      username: p.username,
      password: p.password,
    });
    let record_video = self.record_video.map(|rv| fo::RecordVideoOptions {
      dir: std::path::PathBuf::from(rv.dir),
      size: rv.size.map(|s| fo::VideoSize {
        width: crate::types::f64_to_u32(s.width),
        height: crate::types::f64_to_u32(s.height),
      }),
    });
    let screen = self.screen.map(|s| fo::ScreenSize {
      width: crate::types::f64_to_i64(s.width),
      height: crate::types::f64_to_i64(s.height),
    });
    let service_workers = self.service_workers.and_then(|s| match s.as_str() {
      "allow" => Some(fo::ServiceWorkerPolicy::Allow),
      "block" => Some(fo::ServiceWorkerPolicy::Block),
      _ => None,
    });
    let storage_state = self.storage_state.and_then(|v| match v {
      serde_json::Value::String(s) => Some(fo::StorageStateInput::Path(std::path::PathBuf::from(s))),
      serde_json::Value::Null => None,
      inline => Some(fo::StorageStateInput::Inline(inline)),
    });
    Ok(fo::BrowserContextOptions {
      // Not a public `newContext` option in Playwright either: the
      // attribute is set by `selectors.setTestIdAttribute` or the test
      // runner's `use` bag, which reach the context directly.
      test_id_attribute: None,
      accept_downloads: self.accept_downloads,
      base_url: self.base_url,
      bypass_csp: self.bypass_csp,
      color_scheme,
      contrast,
      device_scale_factor: self.device_scale_factor,
      extra_http_headers: self.extra_http_headers,
      forced_colors,
      geolocation: self.geolocation.map(|g| fo::Geolocation {
        latitude: g.latitude,
        longitude: g.longitude,
        accuracy: g.accuracy.unwrap_or(0.0),
      }),
      has_touch: self.has_touch,
      http_credentials,
      ignore_https_errors: self.ignore_https_errors,
      is_mobile: self.is_mobile,
      java_script_enabled: self.java_script_enabled,
      locale: self.locale,
      offline: self.offline,
      network_conditions,
      cpu_throttling_rate: self.cpu_throttling,
      permissions: self.permissions,
      origin_permissions: Vec::new(),
      proxy,
      record_har: self.record_har.map(RecordHarOptionsJs::into_core),
      record_video,
      reduced_motion,
      screen,
      service_workers,
      storage_state,
      strict_selectors: self.strict_selectors,
      timezone_id: self.timezone_id,
      user_agent: self.user_agent,
      viewport,
    })
  }
}

/// Resolve a `setStorageState` argument into the inline state JSON. A JSON
/// string is treated as a path to a state file written by `storageState({
/// path })`; any other value is the inline `{ cookies, origins }` object.
pub(crate) fn resolve_storage_state_input(input: serde_json::Value) -> std::result::Result<serde_json::Value, String> {
  match input {
    serde_json::Value::String(path) => {
      let text = std::fs::read_to_string(&path).map_err(|e| format!("setStorageState: read {path}: {e}"))?;
      serde_json::from_str(&text).map_err(|e| format!("setStorageState: parse JSON from {path}: {e}"))
    },
    other => Ok(other),
  }
}

/// The `urls?: string | string[]` of `cookies()`.
pub(crate) fn cookie_urls(urls: Option<napi::Either<String, Vec<String>>>) -> Vec<String> {
  match urls {
    None => Vec::new(),
    Some(napi::Either::A(one)) => vec![one],
    Some(napi::Either::B(many)) => many,
  }
}
