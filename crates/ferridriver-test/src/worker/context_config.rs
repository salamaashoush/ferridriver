//! Effective per-test and per-suite browser context configuration lowering.

use crate::config::{ColorSchemeSetting, ContextConfig, TestConfig, ViewportConfig};

#[derive(Clone)]
pub(super) struct EffectiveContextConfig {
  pub(super) context: ContextConfig,
  pub(super) default_viewport: Option<ferridriver_config::browser::ViewportOverride>,
  pub(super) viewport_override: Option<ferridriver_config::browser::ViewportOverride>,
  pub(super) request_base_url: Option<String>,
}

impl EffectiveContextConfig {
  /// Identity of the contexts this config produces, for
  /// [`crate::context_pool`]. Everything `build_context_options` reads
  /// goes in, so two tests share a key exactly when a context built for
  /// one is a valid context for the other. Serializing rather than
  /// hashing keeps a new config field from silently falling out of the
  /// key the way a hand-written `Hash` impl would.
  pub(super) fn pool_key(&self, backend: ferridriver::backend::BackendKind) -> crate::context_pool::PoolKey {
    let parts = serde_json::json!({
      "backend": format!("{backend:?}"),
      "context": &self.context,
      "viewport": match self.viewport_override.as_ref().or(self.default_viewport.as_ref()) {
        None => serde_json::json!("default"),
        Some(ferridriver_config::browser::ViewportOverride::Disabled) => serde_json::Value::Null,
        Some(ferridriver_config::browser::ViewportOverride::Size(size)) => serde_json::json!(size),
      },
      "baseUrl": &self.request_base_url,
    });
    parts.to_string()
  }
}

/// The device and page emulation keys of a spec's `test.use({ ... })`.
pub(super) fn apply_spec_emulation(ctx_config: &mut ContextConfig, opts: &serde_json::Value) {
  if let Some(v) = opts.get("locale").and_then(|v| v.as_str()) {
    ctx_config.locale = Some(v.to_string());
  }
  if let Some(screen) = opts.get("screen").and_then(|v| v.as_object())
    && let (Some(w), Some(h)) = (
      screen.get("width").and_then(serde_json::Value::as_i64),
      screen.get("height").and_then(serde_json::Value::as_i64),
    )
  {
    ctx_config.screen = Some(ViewportConfig { width: w, height: h });
  }
  match opts.get("colorScheme") {
    Some(serde_json::Value::String(scheme)) => {
      ctx_config.color_scheme = Some(ColorSchemeSetting::Scheme(scheme.clone()));
    },
    Some(serde_json::Value::Null) => ctx_config.color_scheme = Some(ColorSchemeSetting::Null),
    _ => {},
  }
  if let Some(v) = opts.get("timezoneId").and_then(|v| v.as_str()) {
    ctx_config.timezone_id = Some(v.to_string());
  }
  if let Some(v) = opts.get("isMobile").and_then(serde_json::Value::as_bool) {
    ctx_config.is_mobile = v;
  }
  if let Some(v) = opts.get("hasTouch").and_then(serde_json::Value::as_bool) {
    ctx_config.has_touch = v;
  }
  if let Some(v) = opts.get("offline").and_then(serde_json::Value::as_bool) {
    ctx_config.offline = v;
  }
  if let Some(v) = opts
    .get("networkConditions")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
  {
    ctx_config.network_conditions = Some(v);
  }
  if let Some(v) = opts.get("cpuThrottling").and_then(serde_json::Value::as_f64) {
    ctx_config.cpu_throttling = Some(v);
  }
  if let Some(v) = opts.get("javaScriptEnabled").and_then(serde_json::Value::as_bool) {
    ctx_config.java_script_enabled = v;
  }
  if let Some(v) = opts.get("bypassCSP").and_then(serde_json::Value::as_bool) {
    ctx_config.bypass_csp = v;
  }
  if let Some(v) = opts.get("userAgent").and_then(|v| v.as_str()) {
    ctx_config.user_agent = Some(v.to_string());
  }
  if let Some(v) = opts.get("testIdAttribute").and_then(|v| v.as_str()) {
    ctx_config.test_id_attribute = Some(v.to_string());
  }
  if let Some(v) = opts.get("deviceScaleFactor").and_then(serde_json::Value::as_f64) {
    ctx_config.device_scale_factor = Some(v);
  }
  // Test-scoped in Playwright, so a spec may set them; `trace`,
  // `video` and `screenshot` are WORKER options there and stay
  // config/project-level here for the same reason.
  if let Some(v) = opts.get("actionTimeout").and_then(serde_json::Value::as_u64) {
    ctx_config.action_timeout = Some(v);
  }
  if let Some(v) = opts.get("navigationTimeout").and_then(serde_json::Value::as_u64) {
    ctx_config.navigation_timeout = Some(v);
  }
  if let Some(v) = opts.get("reducedMotion").and_then(|v| v.as_str()) {
    ctx_config.reduced_motion = Some(v.to_string());
  }
  if let Some(v) = opts.get("forcedColors").and_then(|v| v.as_str()) {
    ctx_config.forced_colors = Some(v.to_string());
  }
}

/// The storage, permission and network keys of a spec's
/// `test.use({ ... })`.
pub(super) fn apply_spec_environment(ctx_config: &mut ContextConfig, opts: &serde_json::Value) {
  if let Some(v) = opts.get("serviceWorkers").and_then(|v| v.as_str()) {
    ctx_config.service_workers = Some(v.to_string());
  }
  if let Some(v) = opts.get("storageState").and_then(|v| v.as_str()) {
    ctx_config.storage_state = Some(v.to_string());
  }
  if let Some(v) = opts.get("acceptDownloads").and_then(serde_json::Value::as_bool) {
    ctx_config.accept_downloads = v;
  }
  if let Some(v) = opts.get("ignoreHTTPSErrors").and_then(serde_json::Value::as_bool) {
    ctx_config.ignore_https_errors = v;
  }
  if let Some(geo) = opts.get("geolocation").and_then(|v| v.as_object())
    && let (Some(lat), Some(lon)) = (
      geo.get("latitude").and_then(serde_json::Value::as_f64),
      geo.get("longitude").and_then(serde_json::Value::as_f64),
    )
  {
    ctx_config.geolocation = Some(crate::config::GeolocationConfig {
      latitude: lat,
      longitude: lon,
      accuracy: geo.get("accuracy").and_then(serde_json::Value::as_f64),
    });
  }
  if let Some(arr) = opts.get("permissions").and_then(|v| v.as_array()) {
    let perms: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    if !perms.is_empty() {
      ctx_config.permissions = perms;
    }
  }
  if let Some(obj) = opts.get("extraHTTPHeaders").and_then(|v| v.as_object()) {
    let headers: std::collections::BTreeMap<String, String> = obj
      .iter()
      .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
      .collect();
    if !headers.is_empty() {
      ctx_config.extra_http_headers = headers;
    }
  }
  if let Some(value) = opts.get("proxy") {
    // A proxy that does not parse must not quietly send the test's
    // traffic direct: the configured one stays, and the run says why.
    match serde_json::from_value(value.clone()) {
      Ok(proxy) => ctx_config.proxy = proxy,
      Err(error) => tracing::warn!(target: "ferridriver::worker", %error, "test.use({{ proxy }}) ignored"),
    }
  }
  if let Some(value) = opts.get("httpCredentials") {
    let entries = value
      .as_array()
      .map_or_else(|| std::slice::from_ref(value), Vec::as_slice);
    let credentials: Vec<_> = entries
      .iter()
      .filter_map(|entry| {
        Some(crate::config::HttpCredentialsConfig {
          username: entry.get("username")?.as_str()?.to_string(),
          password: entry.get("password")?.as_str()?.to_string(),
          origin: entry.get("origin").and_then(|v| v.as_str()).map(String::from),
        })
      })
      .collect();
    if !credentials.is_empty() {
      ctx_config.http_credentials = credentials;
    }
  }
}

pub(super) fn build_effective_context_config(
  config: &TestConfig,
  test: &crate::model::TestCase,
) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.use_options.clone();
  // `test.use({ device })` pre-seeds the keys a config layer's `device`
  // pre-seeds, through the same function — a spec and a config cannot
  // disagree about what a device is.
  let spec_use = test.use_options.as_ref().map(|opts| {
    let mut expanded = opts.as_object().cloned().unwrap_or_default();
    crate::config::expand_device_keys(&mut expanded);
    serde_json::Value::Object(expanded)
  });
  if let Some(ref opts) = spec_use {
    apply_spec_emulation(&mut ctx_config, opts);
    apply_spec_environment(&mut ctx_config, opts);
  }

  let viewport_override = spec_use.as_ref().and_then(|opts| {
    opts.get("viewport").and_then(|v| {
      if v.is_null() {
        return Some(ferridriver_config::browser::ViewportOverride::Disabled);
      }
      let w = v.get("width").and_then(serde_json::Value::as_i64);
      let h = v.get("height").and_then(serde_json::Value::as_i64);
      match (w, h) {
        (Some(w), Some(h)) => Some(ferridriver_config::browser::ViewportOverride::Size(ViewportConfig {
          width: w,
          height: h,
        })),
        _ => None,
      }
    })
  });

  let request_base_url = spec_use
    .as_ref()
    .and_then(|opts| opts.get("baseURL").and_then(|v| v.as_str()).map(String::from))
    .or_else(|| config.base_url.clone())
    .or_else(crate::config::run_base_url);

  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state.clone_from(&config.storage_state);
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config_viewport(&config.browser),
    viewport_override,
    request_base_url,
  }
}

/// The viewport a config asks for.
///
/// `use: { viewport }` is Playwright's spelling and wins when present —
/// `null` included, which is how a suite says "no fixed viewport".
/// `[test.browser].viewport` is the spelling that predates the `use`
/// bag and answers when `use` is silent.
pub(super) fn config_viewport(
  browser: &crate::config::BrowserConfig,
) -> Option<ferridriver_config::browser::ViewportOverride> {
  browser
    .use_options
    .viewport
    .clone()
    .or_else(|| browser.viewport.clone())
}

pub(super) fn build_suite_effective_context_config(config: &TestConfig) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.use_options.clone();
  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state.clone_from(&config.storage_state);
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config_viewport(&config.browser),
    viewport_override: None,
    request_base_url: config.base_url.clone().or_else(crate::config::run_base_url),
  }
}

/// Lower the effective test config into the `BrowserContextOptions` bag
/// passed to `browser.new_context()`. Creation-time options matter:
/// document-time overrides (locale, userAgent, timezone) must be in the
/// context's stashed options BEFORE the first page's process spawns —
/// `WebKit` in particular latches languages at target creation and a
/// post-attach `Playwright.setLanguages` never reaches the already
/// running web process.
pub(super) fn build_context_options(
  effective: &EffectiveContextConfig,
  output_dir: &std::path::Path,
  backend_kind: ferridriver::backend::BackendKind,
) -> ferridriver::options::BrowserContextOptions {
  let ctx_config = &effective.context;
  let mut opts = ferridriver::options::BrowserContextOptions::default();
  // Playwright WebKit rejects several context-options fields outright
  // on launchPersistentContext; degrade silently when the user hasn't
  // explicitly opted in.
  let is_webkit = matches!(backend_kind, ferridriver::backend::BackendKind::WebKit);

  let viewport = effective
    .viewport_override
    .as_ref()
    .or(effective.default_viewport.as_ref());
  opts.viewport = match viewport {
    Some(ferridriver_config::browser::ViewportOverride::Disabled) => ferridriver::options::ViewportOption::Null,
    Some(ferridriver_config::browser::ViewportOverride::Size(vp)) => ferridriver::options::ViewportOption::Size {
      width: vp.width,
      height: vp.height,
    },
    None => ferridriver::options::ViewportOption::Default,
  };
  opts.device_scale_factor = ctx_config.device_scale_factor;
  opts.screen = ctx_config.screen.as_ref().map(|s| ferridriver::options::ScreenSize {
    width: s.width,
    height: s.height,
  });
  if ctx_config.is_mobile {
    opts.is_mobile = Some(true);
  }
  if ctx_config.has_touch {
    opts.has_touch = Some(true);
  }
  opts.color_scheme = ctx_config.color_scheme.as_ref().map_or(
    ferridriver::options::MediaOverride::Unchanged,
    ColorSchemeSetting::media_override,
  );
  opts.reduced_motion = ctx_config.reduced_motion.clone().into();
  opts.forced_colors = ctx_config.forced_colors.clone().into();
  opts.locale.clone_from(&ctx_config.locale);
  opts.timezone_id.clone_from(&ctx_config.timezone_id);
  if let Some(ref geo) = ctx_config.geolocation {
    opts.geolocation = Some(ferridriver::options::Geolocation {
      latitude: geo.latitude,
      longitude: geo.longitude,
      accuracy: geo.accuracy.unwrap_or(0.0),
    });
  }
  if ctx_config.offline {
    opts.offline = Some(true);
  }
  opts.network_conditions = ctx_config.network_conditions;
  opts.cpu_throttling_rate = ctx_config.cpu_throttling;
  if !ctx_config.permissions.is_empty() {
    opts.permissions = Some(ctx_config.permissions.clone());
  }
  if !ctx_config.extra_http_headers.is_empty() {
    opts.extra_http_headers = Some(
      ctx_config
        .extra_http_headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    );
  }
  opts.user_agent.clone_from(&ctx_config.user_agent);
  opts.test_id_attribute.clone_from(&ctx_config.test_id_attribute);
  // Plumb the test config's `baseURL` into the BrowserContext bag so
  // `page.goto('/route')` resolves against it. Previously the value
  // was only stored as `request_base_url` for the API-request
  // fixture, leaving relative `page.goto` paths to fail with "Cannot
  // navigate to invalid URL" — Playwright resolves these via the
  // context's baseURL option, mirror that.
  if opts.base_url.is_none() {
    opts.base_url.clone_from(&effective.request_base_url);
  }
  if !ctx_config.java_script_enabled {
    opts.java_script_enabled = Some(false);
  }
  if ctx_config.bypass_csp && !is_webkit {
    opts.bypass_csp = Some(true);
  }
  if ctx_config.ignore_https_errors && !is_webkit {
    opts.ignore_https_errors = Some(true);
  }
  // Note: `ctx_config.accept_downloads` defaults to `true` (Playwright
  // parity). We deliberately don't pass that through to
  // `BrowserContextOptions.accept_downloads` here — doing so makes
  // `apply_context_options` fire `Browser.setDownloadBehavior` on
  // every per-test page, which is ~3-5ms per test on the bench's
  // tight loop. The page-level lazy `enable_download_behavior` (fired
  // on first `wait_for_download` / `page.on('download')`) handles the
  // CDP command when a test actually needs it. Tests that opt OUT
  // (`acceptDownloads: false`) still flow through, since opts.deny is
  // an explicit decision the bag has to encode.
  if !ctx_config.accept_downloads && !is_webkit {
    opts.accept_downloads = Some(false);
  }
  if ctx_config.accept_downloads && !is_webkit {
    let _ = std::fs::create_dir_all(output_dir.join("downloads"));
  }
  if !ctx_config.http_credentials.is_empty() {
    opts.http_credentials = Some(core_http_credentials(&ctx_config.http_credentials));
  }
  opts.proxy = ctx_config.proxy.as_ref().map(crate::config::ProxyConfig::to_engine);
  if ctx_config.service_workers.as_deref() == Some("block") {
    opts.service_workers = Some(ferridriver::options::ServiceWorkerPolicy::Block);
  }
  if let Some(ss_path) = ctx_config.storage_state.as_deref() {
    opts.storage_state = Some(ferridriver::options::StorageStateInput::Path(ss_path.into()));
  }

  opts
}

/// Worker-scope `request` fixture. Builds one [`HttpClient`] per worker
/// so the underlying reqwest connection pool, TLS context, and cookie
/// jar are reused across every test on this worker — saves the per-test
/// `reqwest::Client::builder().build()` cost (~1-10ms each on the bench).
///
/// `base_url` is captured from the worker's config; per-test
/// `use_options.base_url` overrides aren't honored at this scope. Tests
/// that need a different base URL should construct an `HttpClient`
/// inside the test body, or we expose a per-test override fixture
/// later (Playwright's `request` fixture has the same worker-scoped
/// shape — `playwright/types/test.d.ts` `APIRequestContext`).
pub(super) fn core_http_credentials(
  configured: &[crate::config::HttpCredentialsConfig],
) -> Vec<ferridriver::options::HttpCredentials> {
  configured
    .iter()
    .map(|creds| ferridriver::options::HttpCredentials {
      username: creds.username.clone(),
      password: creds.password.clone(),
      origin: creds.origin.clone(),
      send: None,
    })
    .collect()
}
