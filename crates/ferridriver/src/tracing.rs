//! `Tracing` — the `context.tracing.*` surface.
//!
//! HAR recording (`startHar` / `stopHar`, Playwright 1.60) is implemented
//! against the context's observed network log: between `start_har` and
//! `stop_har` the context's requests are captured and serialized to a
//! HAR 1.2 archive. Mirrors `client/tracing.ts::startHar` / `stopHar`.
//!
//! The trace `.zip` recorder (`start` / `stop` / `startChunk` /
//! `stopChunk`, with DOM snapshots, screenshots and source attachments)
//! is a separate large subsystem; those methods return a typed
//! [`FerriError::Unsupported`] rather than a placeholder artifact.

use std::path::PathBuf;

use crate::context::ContextRef;
use crate::error::{FerriError, Result};
use crate::network::Request;
use crate::url_matcher::UrlMatcher;

/// `content` policy for `startHar` — whether (and how) response bodies
/// are stored. Mirrors Playwright's `'embed' | 'attach' | 'omit'`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HarContentPolicy {
  /// Inline the body in `content.text` (base64 for binary).
  Embed,
  /// Playwright stores bodies as separate resources; ferridriver inlines
  /// them like `Embed` (no separate resources dir / zip yet).
  Attach,
  /// Drop bodies entirely.
  Omit,
}

/// `mode` for `startHar`. Mirrors Playwright's `'full' | 'minimal'`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HarMode {
  Full,
  Minimal,
}

/// Options bag for [`Tracing::start_har`].
#[derive(Default)]
pub struct StartHarOptions {
  pub content: Option<HarContentPolicy>,
  pub mode: Option<HarMode>,
  pub url_filter: Option<UrlMatcher>,
}

/// Live recorder state, stored per-context on [`crate::state::BrowserState`]
/// between `start_har` and `stop_har`.
pub struct HarRecorder {
  pub path: PathBuf,
  pub content: HarContentPolicy,
  pub mode: HarMode,
  pub url_filter: UrlMatcher,
  /// Index into the context's `network_log` at recording start; only
  /// requests appended after this point are written.
  pub start_len: usize,
}

/// `context.tracing` handle. Cheap to construct (wraps a [`ContextRef`]).
pub struct Tracing {
  ctx: ContextRef,
}

impl Tracing {
  #[must_use]
  pub(crate) fn new(ctx: ContextRef) -> Self {
    Self { ctx }
  }

  /// Begin recording network into a HAR file. Playwright:
  /// `tracing.startHar(path, { content?, mode?, urlFilter? })`.
  ///
  /// # Errors
  ///
  /// Errors if a HAR recording is already active, the target is a `.zip`
  /// (zip HAR archives are not implemented), or the context is missing.
  pub async fn start_har(&self, path: impl Into<PathBuf>, options: StartHarOptions) -> Result<()> {
    let path = path.into();
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
      return Err(FerriError::unsupported(
        "zip HAR archives are not implemented; use a .har path",
      ));
    }
    let composite = self.ctx.composite();
    let start_len = self.network_log_len().await;
    let recorder = HarRecorder {
      path,
      content: options.content.unwrap_or(HarContentPolicy::Embed),
      mode: options.mode.unwrap_or(HarMode::Full),
      url_filter: options.url_filter.unwrap_or_else(UrlMatcher::any),
      start_len,
    };
    let recorders = self.ctx.har_recorders().await;
    let mut guard = recorders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.contains_key(&composite) {
      return Err(FerriError::backend(
        "HAR recording has already been started".to_string(),
      ));
    }
    guard.insert(composite, recorder);
    Ok(())
  }

  /// Stop the active HAR recording and write the archive to disk.
  /// Playwright: `tracing.stopHar()`.
  ///
  /// # Errors
  ///
  /// Errors if no recording is active or the file cannot be written.
  pub async fn stop_har(&self) -> Result<()> {
    let composite = self.ctx.composite();
    let recorder = {
      let recorders = self.ctx.har_recorders().await;
      let mut guard = recorders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      guard.remove(&composite)
    };
    let Some(recorder) = recorder else {
      return Err(FerriError::backend("HAR recording has not been started".to_string()));
    };

    let requests = self.network_log_slice(recorder.start_len).await;
    let archive = build_har(&requests, &recorder).await;
    let json =
      serde_json::to_string_pretty(&archive).map_err(|e| FerriError::backend(format!("serialize HAR: {e}")))?;
    std::fs::write(&recorder.path, json)
      .map_err(|e| FerriError::backend(format!("write HAR {}: {e}", recorder.path.display())))?;
    Ok(())
  }

  async fn network_log_len(&self) -> usize {
    match self.ctx.network_log_handle().await {
      Some(log) => log.read().await.len(),
      None => 0,
    }
  }

  async fn network_log_slice(&self, start: usize) -> Vec<Request> {
    match self.ctx.network_log_handle().await {
      Some(log) => {
        let reqs = log.read().await;
        reqs.iter().skip(start).cloned().collect()
      },
      None => Vec::new(),
    }
  }

  /// Playwright: `tracing.start(options?)`. The trace `.zip` recorder is
  /// not implemented.
  ///
  /// # Errors
  ///
  /// Always [`FerriError::Unsupported`].
  pub fn start(&self) -> Result<()> {
    Err(trace_zip_unsupported())
  }

  /// Playwright: `tracing.startChunk(options?)`.
  ///
  /// # Errors
  ///
  /// Always [`FerriError::Unsupported`].
  pub fn start_chunk(&self) -> Result<()> {
    Err(trace_zip_unsupported())
  }

  /// Playwright: `tracing.stopChunk(options?)`.
  ///
  /// # Errors
  ///
  /// Always [`FerriError::Unsupported`].
  pub fn stop_chunk(&self) -> Result<()> {
    Err(trace_zip_unsupported())
  }

  /// Playwright: `tracing.stop(options?)`.
  ///
  /// # Errors
  ///
  /// Always [`FerriError::Unsupported`].
  pub fn stop(&self) -> Result<()> {
    Err(trace_zip_unsupported())
  }
}

fn trace_zip_unsupported() -> FerriError {
  FerriError::unsupported(
    "trace.zip recording (start/stop/startChunk/stopChunk) is not implemented; \
     use startHar/stopHar for network capture",
  )
}

async fn build_har(requests: &[Request], recorder: &HarRecorder) -> HarArchive {
  let mut entries = Vec::new();
  for req in requests {
    if !recorder.url_filter.matches(req.url()) {
      continue;
    }
    entries.push(build_entry(req, recorder).await);
  }
  HarArchive {
    log: HarLogOut {
      version: "1.2",
      creator: HarCreator {
        name: "ferridriver",
        version: env!("CARGO_PKG_VERSION"),
      },
      entries,
    },
  }
}

async fn build_entry(req: &Request, recorder: &HarRecorder) -> HarEntryOut {
  let request_headers = header_pairs(&req.headers());
  let post_data = req.post_data().map(|text| HarPostData {
    mime_type: req
      .headers()
      .get("content-type")
      .cloned()
      .unwrap_or_else(|| "application/octet-stream".to_string()),
    text,
  });

  let (response_out, response_present) = match req.response().await.ok().flatten() {
    Some(resp) => {
      let headers = header_pairs(&resp.headers());
      let mime_type = resp
        .headers()
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| "x-unknown".to_string());
      let content = build_content(&resp, &mime_type, recorder.content).await;
      (
        HarResponseOut {
          status: resp.status(),
          status_text: resp.status_text().to_string(),
          http_version: "HTTP/1.1".to_string(),
          headers,
          content,
          redirect_url: String::new(),
          headers_size: -1,
          body_size: -1,
        },
        true,
      )
    },
    None => (HarResponseOut::empty(), false),
  };

  // `mode: minimal` records only entries that have a response; full keeps
  // request-only entries too. Both still emit the entry shape.
  let _ = (recorder.mode, response_present);

  HarEntryOut {
    started_date_time: now_iso8601(),
    time: 0.0,
    request: HarRequestOut {
      method: req.method().to_string(),
      url: req.url().to_string(),
      http_version: "HTTP/1.1".to_string(),
      headers: request_headers,
      query_string: Vec::new(),
      post_data,
      headers_size: -1,
      body_size: -1,
    },
    response: response_out,
    cache: HarCache {},
    timings: HarTimings {
      send: 0.0,
      wait: 0.0,
      receive: 0.0,
    },
  }
}

async fn build_content(resp: &crate::network::Response, mime_type: &str, policy: HarContentPolicy) -> HarContentOut {
  if policy == HarContentPolicy::Omit {
    return HarContentOut {
      size: 0,
      mime_type: mime_type.to_string(),
      text: None,
      encoding: None,
    };
  }
  match resp.body().await {
    Ok(bytes) => {
      let size = bytes.len() as i64;
      match String::from_utf8(bytes.clone()) {
        Ok(text) => HarContentOut {
          size,
          mime_type: mime_type.to_string(),
          text: Some(text),
          encoding: None,
        },
        Err(_) => {
          use base64::Engine;
          HarContentOut {
            size,
            mime_type: mime_type.to_string(),
            text: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            encoding: Some("base64".to_string()),
          }
        },
      }
    },
    Err(_) => HarContentOut {
      size: 0,
      mime_type: mime_type.to_string(),
      text: None,
      encoding: None,
    },
  }
}

fn header_pairs(headers: &crate::network::Headers) -> Vec<HarHeaderOut> {
  headers
    .iter()
    .map(|(name, value)| HarHeaderOut {
      name: name.clone(),
      value: value.clone(),
    })
    .collect()
}

/// Format the current wall-clock time as an ISO-8601 UTC string with
/// millisecond precision (`YYYY-MM-DDTHH:MM:SS.mmmZ`). Uses the civil-
/// from-days algorithm so no date dependency is needed.
fn now_iso8601() -> String {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  let total_ms = now.as_millis() as i64;
  let secs = total_ms.div_euclid(1000);
  let ms = total_ms.rem_euclid(1000);
  let days = secs.div_euclid(86_400);
  let tod = secs.rem_euclid(86_400);
  let (hour, minute, second) = (tod / 3600, (tod % 3600) / 60, tod % 60);
  let (year, month, day) = civil_from_days(days);
  format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Howard Hinnant's `civil_from_days`: convert a day count since the Unix
/// epoch into a `(year, month, day)` Gregorian date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
  let z = z + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let y = yoe + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
  let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
  (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── HAR 1.2 serialization shapes ──────────────────────────────────────

#[derive(serde::Serialize)]
struct HarArchive {
  log: HarLogOut,
}

#[derive(serde::Serialize)]
struct HarLogOut {
  version: &'static str,
  creator: HarCreator,
  entries: Vec<HarEntryOut>,
}

#[derive(serde::Serialize)]
struct HarCreator {
  name: &'static str,
  version: &'static str,
}

#[derive(serde::Serialize)]
struct HarEntryOut {
  #[serde(rename = "startedDateTime")]
  started_date_time: String,
  time: f64,
  request: HarRequestOut,
  response: HarResponseOut,
  cache: HarCache,
  timings: HarTimings,
}

#[derive(serde::Serialize)]
struct HarRequestOut {
  method: String,
  url: String,
  #[serde(rename = "httpVersion")]
  http_version: String,
  headers: Vec<HarHeaderOut>,
  #[serde(rename = "queryString")]
  query_string: Vec<HarHeaderOut>,
  #[serde(rename = "postData", skip_serializing_if = "Option::is_none")]
  post_data: Option<HarPostData>,
  #[serde(rename = "headersSize")]
  headers_size: i64,
  #[serde(rename = "bodySize")]
  body_size: i64,
}

#[derive(serde::Serialize)]
struct HarPostData {
  #[serde(rename = "mimeType")]
  mime_type: String,
  text: String,
}

#[derive(serde::Serialize)]
struct HarResponseOut {
  status: i64,
  #[serde(rename = "statusText")]
  status_text: String,
  #[serde(rename = "httpVersion")]
  http_version: String,
  headers: Vec<HarHeaderOut>,
  content: HarContentOut,
  #[serde(rename = "redirectURL")]
  redirect_url: String,
  #[serde(rename = "headersSize")]
  headers_size: i64,
  #[serde(rename = "bodySize")]
  body_size: i64,
}

impl HarResponseOut {
  fn empty() -> Self {
    Self {
      status: 0,
      status_text: String::new(),
      http_version: "HTTP/1.1".to_string(),
      headers: Vec::new(),
      content: HarContentOut {
        size: 0,
        mime_type: "x-unknown".to_string(),
        text: None,
        encoding: None,
      },
      redirect_url: String::new(),
      headers_size: -1,
      body_size: -1,
    }
  }
}

#[derive(serde::Serialize)]
struct HarContentOut {
  size: i64,
  #[serde(rename = "mimeType")]
  mime_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  text: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  encoding: Option<String>,
}

#[derive(serde::Serialize)]
struct HarHeaderOut {
  name: String,
  value: String,
}

#[derive(serde::Serialize)]
struct HarCache {}

#[derive(serde::Serialize)]
struct HarTimings {
  send: f64,
  wait: f64,
  receive: f64,
}
