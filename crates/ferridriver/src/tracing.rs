//! `Tracing` — the `context.tracing.*` surface.
//!
//! HAR recording (`startHar` / `stopHar`, Playwright 1.60) is implemented
//! against the context's observed network log: between `start_har` and
//! `stop_har` the context's requests are captured and serialized to a
//! HAR 1.2 archive — a plain `.har` JSON file (bodies inlined or attached
//! to a resources directory) or a `.zip` packing `har.har` plus
//! `<sha1>.<ext>` body entries. Mirrors `client/tracing.ts::startHar` /
//! `stopHar` and `server/har/harRecorder.ts`. The same recorder backs
//! `routeFromHAR(update: true)`, flushed when the context closes.
//!
//! The trace `.zip` recorder (`start` / `stop` / `startChunk` /
//! `stopChunk`) lives in [`crate::trace`] and emits Playwright's
//! format VERSION 8; this module hosts the `context.tracing` handle
//! that fronts both recorders.

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
  /// Store bodies as separate resources named `<sha1>.<ext>`, referenced
  /// from the entry via `content._file` — inside the archive for a
  /// `.zip` HAR, next to the `.har` file (or in `resourcesDir`)
  /// otherwise.
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
  /// Where `attach`ed bodies are written for a non-zip HAR. Defaults to
  /// the HAR file's directory. Incompatible with a `.zip` path.
  pub resources_dir: Option<PathBuf>,
}

/// Live recorder state, stored per-context on [`crate::state::BrowserState`]
/// between `start_har` and `stop_har` (or, for
/// `routeFromHAR(update: true)`, until context close).
pub struct HarRecorder {
  pub path: PathBuf,
  pub content: HarContentPolicy,
  pub mode: HarMode,
  pub url_filter: UrlMatcher,
  /// Resource directory override for non-zip `attach` recordings.
  pub resources_dir: Option<PathBuf>,
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
  /// `tracing.startHar(path, { content?, mode?, urlFilter?, resourcesDir? })`.
  ///
  /// A `.zip` path packs the archive as `har.har` plus one `<sha1>.<ext>`
  /// entry per attached body; the default `content` policy is `attach`
  /// for `.zip` and `embed` otherwise (mirrors `client/tracing.ts:105`).
  ///
  /// # Errors
  ///
  /// Errors if a HAR recording is already active, `resourcesDir` is
  /// combined with a `.zip` path, or the context is missing.
  pub async fn start_har(&self, path: impl Into<PathBuf>, options: StartHarOptions) -> Result<()> {
    let path = path.into();
    let is_zip = is_zip_path(&path);
    if is_zip && options.resources_dir.is_some() {
      return Err(FerriError::backend(
        "resourcesDir option is not compatible with a .zip har file".to_string(),
      ));
    }
    let default_content = if is_zip {
      HarContentPolicy::Attach
    } else {
      HarContentPolicy::Embed
    };
    let composite = self.ctx.composite();
    let start_len = self.network_log_len().await;
    let recorder = HarRecorder {
      path,
      content: options.content.unwrap_or(default_content),
      mode: options.mode.unwrap_or(HarMode::Full),
      url_filter: options.url_filter.unwrap_or_else(UrlMatcher::any),
      resources_dir: options.resources_dir,
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
    flush_recorder(&recorder, &requests).await
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

  /// Playwright: `tracing.start(options?: { name?, title?, screenshots?,
  /// snapshots?, sources? })`. Starts recording a Playwright-format
  /// (VERSION 8) trace; write it with [`Self::stop`]. See
  /// [`crate::trace`] for the exact coverage (actions, DOM snapshots,
  /// film strip, console, page events, network, embedded sources).
  ///
  /// # Errors
  ///
  /// Errors if tracing is already started on this context.
  pub async fn start(&self, options: crate::trace::TracingStartOptions) -> Result<()> {
    let composite = self.ctx.composite();
    let (browser_name, context_options) = {
      let state = self.ctx.state().read().await;
      let browser_name = match state.backend_kind() {
        crate::backend::BackendKind::CdpPipe | crate::backend::BackendKind::CdpRaw => "chromium",
        crate::backend::BackendKind::WebKit => "webkit",
        crate::backend::BackendKind::Bidi => "firefox",
      };
      let mut context_options = serde_json::Map::new();
      if let Some(viewport) = state.default_viewport.as_ref() {
        context_options.insert(
          "viewport".to_string(),
          serde_json::json!({ "width": viewport.width, "height": viewport.height }),
        );
        context_options.insert(
          "deviceScaleFactor".to_string(),
          serde_json::json!(viewport.device_scale_factor),
        );
      }
      (browser_name, serde_json::Value::Object(context_options))
    };
    let network_len = self.network_log_len().await;
    let recorder = std::sync::Arc::new(crate::trace::TraceRecorder::new(
      &options,
      browser_name.to_string(),
      context_options,
      network_len,
    )?);
    crate::trace::install_recorder(&composite, std::sync::Arc::clone(&recorder))?;
    if recorder.screenshots {
      self.start_screencast_pumps(&recorder).await;
    }
    if recorder.snapshots {
      self.install_snapshot_streamer().await;
    }
    Ok(())
  }

  /// Install the DOM snapshot streamer: registered as a context
  /// init-script (future documents, all frames) and evaluated into the
  /// current document of every open page. Child frames of documents
  /// that predate `start` pick the streamer up on their next
  /// navigation. A streamer left over from a previous recording still
  /// holds node refs into a file this trace does not contain — the
  /// capture expression's epoch check self-resets it on the next
  /// capture ([`crate::snapshotter`]), with no boundary-time evaluate
  /// into every frame (which would stall on dead frames).
  async fn install_snapshot_streamer(&self) {
    let source = crate::snapshotter::install_source();
    if let Err(e) = self.ctx.add_init_script_source(source.clone()).await {
      tracing::warn!(target: "ferridriver::trace", "snapshot streamer init-script failed: {e}");
      return;
    }
    let pages = self.context_pages().await;
    for page in &pages {
      if let Err(e) = page.evaluate(&source).await {
        tracing::debug!(target: "ferridriver::trace", "snapshot streamer eval skipped: {e}");
      }
    }
  }

  async fn context_pages(&self) -> Vec<crate::backend::AnyPage> {
    let state = self.ctx.state().read().await;
    state
      .context(self.ctx.name())
      .map(|c| c.pages.clone())
      .unwrap_or_default()
  }

  /// Playwright: `tracing.startChunk(options?)`. Resets the chunk-local
  /// event/resource buffers; the recorder keeps running.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started.
  pub async fn start_chunk(&self) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before starting a new chunk".to_string()))?;
    recorder.start_chunk(self.network_log_len().await);
    Ok(())
  }

  /// Playwright: `tracing.stopChunk(options?: { path? })`. Exports the
  /// current chunk (when `path` is given) and starts a fresh one;
  /// tracing keeps running.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started or the export fails.
  pub async fn stop_chunk(&self, options: crate::trace::TracingStopOptions) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before stopping".to_string()))?;
    if let Some(path) = options.path {
      let network = self.trace_network_entries(&recorder).await;
      recorder.export(&path, &network)?;
    }
    recorder.start_chunk(self.network_log_len().await);
    Ok(())
  }

  /// Playwright: `tracing.stop(options?: { path? })`. Ends the
  /// recording, writing `trace.zip` when `path` is given.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started or the export fails.
  pub async fn stop(&self, options: crate::trace::TracingStopOptions) -> Result<()> {
    let composite = self.ctx.composite();
    let recorder = crate::trace::take_recorder(&composite)
      .ok_or_else(|| FerriError::backend("Must start tracing before stopping".to_string()))?;
    recorder.stop_screencasts();
    if let Some(path) = options.path {
      let network = self.trace_network_entries(&recorder).await;
      recorder.export(&path, &network)?;
    }
    Ok(())
  }

  /// Serialize the context's network log (from the recorder's chunk
  /// start) into HAR entry values for `trace.network`, stamping the
  /// `_monotonicTime` the viewer sorts and correlates by.
  async fn trace_network_entries(&self, recorder: &crate::trace::TraceRecorder) -> Vec<serde_json::Value> {
    let start = usize::try_from(recorder.network_start_len.load(std::sync::atomic::Ordering::SeqCst)).unwrap_or(0);
    let requests = self.network_log_slice(start).await;
    // Attach bodies as sha1-named resources so snapshot subresources
    // (stylesheets, images) resolve in the viewer — its snapshot server
    // reads `response.content._sha1` (`snapshotServer.ts`).
    let ephemeral = HarRecorder {
      path: std::path::PathBuf::new(),
      content: HarContentPolicy::Attach,
      mode: HarMode::Full,
      url_filter: UrlMatcher::any(),
      resources_dir: None,
      start_len: start,
    };
    // frame id -> `page@<id>` map so entries carry `pageref` /
    // `_frameref` — the viewer prefers a same-frame resource when a
    // snapshot subresource URL matches several responses
    // (`snapshotRenderer.ts::resourceByUrl`).
    let mut frame_to_page: rustc_hash::FxHashMap<String, String> = rustc_hash::FxHashMap::default();
    {
      let state = self.ctx.state().read().await;
      let pages = state
        .context(self.ctx.name())
        .map(|c| c.pages.clone())
        .unwrap_or_default();
      drop(state);
      for page in pages {
        let page_ref = crate::trace::trace_page_id(&page);
        let frame_ids = {
          let cache = page
            .frame_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          cache.all_frame_ids()
        };
        for frame_id in frame_ids {
          frame_to_page.insert(frame_id.to_string(), page_ref.clone());
        }
      }
    }

    let mut attachments = Vec::new();
    let mut entries = Vec::new();
    for req in &requests {
      let entry = build_entry(req, &ephemeral, &mut attachments).await;
      if let Ok(mut value) = serde_json::to_value(&entry) {
        if let Some(content) = value.pointer_mut("/response/content").and_then(|c| c.as_object_mut()) {
          if let Some(file) = content.remove("_file") {
            content.insert("_sha1".to_string(), file);
          }
        }
        if let Some(obj) = value.as_object_mut() {
          // Capture time (epoch ms) mapped onto the recorder's
          // monotonic timeline — requests are anchored at creation, so
          // the sample is always present.
          let start_time = req.timing().start_time;
          let monotonic = if start_time > 0.0 {
            recorder.monotonic_of_wall_ms(start_time)
          } else {
            recorder.monotonic_ms()
          };
          obj.insert("_monotonicTime".to_string(), serde_json::json!(monotonic));
          if let Some(frame_id) = req.frame_id() {
            obj.insert("_frameref".to_string(), serde_json::json!(frame_id));
            if let Some(page_ref) = frame_to_page.get(frame_id) {
              obj.insert("pageref".to_string(), serde_json::json!(page_ref));
            }
          }
        }
        entries.push(value);
      }
    }
    for (name, bytes) in attachments {
      recorder.push_resource(&crate::trace::TraceResource { name, bytes });
    }
    entries
  }

  /// Start a screencast pump on every open page, feeding JPEG frames
  /// into the trace (Playwright film strip).
  async fn start_screencast_pumps(&self, recorder: &std::sync::Arc<crate::trace::TraceRecorder>) {
    let pages = {
      let state = self.ctx.state().read().await;
      state
        .context(self.ctx.name())
        .map(|c| c.pages.clone())
        .unwrap_or_default()
    };
    for page in pages {
      crate::trace::spawn_screencast_pump(recorder, &page).await;
    }
  }
}

/// Whether the recorder writes a zip archive (`.zip` extension).
fn is_zip_path(path: &std::path::Path) -> bool {
  path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Serialize the recorded requests and write the archive to the
/// recorder's path — a zip (`har.har` + `<sha1>.<ext>` body entries)
/// for a `.zip` path, a JSON file plus a resources directory of
/// attached bodies otherwise. Shared by [`Tracing::stop_har`] and the
/// context-close flush of `routeFromHAR(update: true)` recorders.
///
/// # Errors
///
/// Errors if serialization or any filesystem write fails.
pub(crate) async fn flush_recorder(recorder: &HarRecorder, requests: &[Request]) -> Result<()> {
  let mut attachments: Vec<(String, Vec<u8>)> = Vec::new();
  let archive = build_har(requests, recorder, &mut attachments).await;
  let json = serde_json::to_string_pretty(&archive).map_err(|e| FerriError::backend(format!("serialize HAR: {e}")))?;

  if is_zip_path(&recorder.path) {
    use std::io::Write;
    let file = std::fs::File::create(&recorder.path)
      .map_err(|e| FerriError::backend(format!("create HAR zip {}: {e}", recorder.path.display())))?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zip_err = |e: zip::result::ZipError| FerriError::backend(format!("write HAR zip: {e}"));
    writer.start_file("har.har", opts).map_err(zip_err)?;
    writer
      .write_all(json.as_bytes())
      .map_err(|e| FerriError::backend(format!("write HAR zip: {e}")))?;
    let mut written = rustc_hash::FxHashSet::default();
    for (name, bytes) in &attachments {
      if !written.insert(name.clone()) {
        continue;
      }
      writer.start_file(name.as_str(), opts).map_err(zip_err)?;
      writer
        .write_all(bytes)
        .map_err(|e| FerriError::backend(format!("write HAR zip: {e}")))?;
    }
    writer.finish().map_err(zip_err)?;
    return Ok(());
  }

  std::fs::write(&recorder.path, json)
    .map_err(|e| FerriError::backend(format!("write HAR {}: {e}", recorder.path.display())))?;
  if !attachments.is_empty() {
    let resources_dir = recorder.resources_dir.clone().unwrap_or_else(|| {
      recorder
        .path
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
    });
    std::fs::create_dir_all(&resources_dir)
      .map_err(|e| FerriError::backend(format!("create HAR resources dir {}: {e}", resources_dir.display())))?;
    let mut written = rustc_hash::FxHashSet::default();
    for (name, bytes) in &attachments {
      if !written.insert(name.clone()) {
        continue;
      }
      let target = resources_dir.join(name);
      std::fs::write(&target, bytes)
        .map_err(|e| FerriError::backend(format!("write HAR resource {}: {e}", target.display())))?;
    }
  }
  Ok(())
}

async fn build_har(
  requests: &[Request],
  recorder: &HarRecorder,
  attachments: &mut Vec<(String, Vec<u8>)>,
) -> HarArchive {
  let mut entries = Vec::new();
  for req in requests {
    if !recorder.url_filter.matches(req.url()) {
      continue;
    }
    entries.push(build_entry(req, recorder, attachments).await);
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

async fn build_entry(req: &Request, recorder: &HarRecorder, attachments: &mut Vec<(String, Vec<u8>)>) -> HarEntryOut {
  let request_headers = header_pairs(&req.headers());
  let post_data = req.post_data().map(|text| HarPostData {
    mime_type: header_value(&req.headers(), "content-type").unwrap_or_else(|| "application/octet-stream".to_string()),
    text,
  });

  let (response_out, response_present) = match req.response().await.ok().flatten() {
    Some(resp) => {
      let headers = header_pairs(&resp.headers());
      let mime_type = header_value(&resp.headers(), "content-type").unwrap_or_else(|| "x-unknown".to_string());
      let content = build_content(&resp, &mime_type, recorder.content, attachments).await;
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

  let _ = response_present;
  // `mode: minimal` omits timing detail (Playwright's slimMode sets
  // omitTiming, encoded as -1 per HAR convention); full derives the
  // phases from the backend timing samples exactly like
  // `harTracer.ts`: `wait = responseStart - requestStart`,
  // `receive = responseEnd - responseStart`, `send: 0`, `-1` when the
  // sample is absent.
  let timing = req.timing();
  let timings = if recorder.mode == HarMode::Minimal {
    HarTimings {
      send: -1.0,
      wait: -1.0,
      receive: -1.0,
    }
  } else {
    let wait = if timing.response_start >= 0.0 && timing.request_start >= 0.0 {
      (timing.response_start - timing.request_start).max(0.0)
    } else {
      -1.0
    };
    let receive = if timing.response_end >= 0.0 && timing.response_start >= 0.0 {
      (timing.response_end - timing.response_start).max(0.0)
    } else {
      -1.0
    };
    HarTimings {
      send: 0.0,
      wait,
      receive,
    }
  };
  let started_date_time = if timing.start_time > 0.0 {
    // Epoch ms stay far below 2^53 — exact in f64, in-range for i64.
    #[allow(clippy::cast_possible_truncation)]
    let ms = timing.start_time as i64;
    epoch_ms_to_iso8601(ms)
  } else {
    now_iso8601()
  };
  let total_time = if timing.response_end >= 0.0 {
    timing.response_end
  } else {
    0.0
  };

  HarEntryOut {
    started_date_time,
    time: total_time,
    request: HarRequestOut {
      method: req.method().to_string(),
      url: req.url().to_string(),
      http_version: "HTTP/1.1".to_string(),
      headers: request_headers,
      query_string: query_pairs(req.url()),
      post_data,
      headers_size: -1,
      body_size: -1,
    },
    response: response_out,
    cache: HarCache {},
    timings,
  }
}

async fn build_content(
  resp: &crate::network::Response,
  mime_type: &str,
  policy: HarContentPolicy,
  attachments: &mut Vec<(String, Vec<u8>)>,
) -> HarContentOut {
  if policy == HarContentPolicy::Omit {
    return HarContentOut {
      size: 0,
      mime_type: mime_type.to_string(),
      text: None,
      encoding: None,
      file: None,
    };
  }
  match resp.body().await {
    Ok(bytes) => {
      let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
      if policy == HarContentPolicy::Attach {
        // `<sha1hex>.<ext>` naming mirrors `harTracer.ts:563` —
        // `calculateSha1(buffer) + '.' + (mime.getExtension(...) || 'dat')`.
        let name = format!("{}.{}", sha1_hex(&bytes), mime_extension(mime_type));
        attachments.push((name.clone(), bytes));
        return HarContentOut {
          size,
          mime_type: mime_type.to_string(),
          text: None,
          encoding: None,
          file: Some(name),
        };
      }
      if let Ok(text) = std::str::from_utf8(&bytes) {
        HarContentOut {
          size,
          mime_type: mime_type.to_string(),
          text: Some(text.to_string()),
          encoding: None,
          file: None,
        }
      } else {
        use base64::Engine;
        HarContentOut {
          size,
          mime_type: mime_type.to_string(),
          text: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
          encoding: Some("base64".to_string()),
          file: None,
        }
      }
    },
    Err(_) => HarContentOut {
      size: 0,
      mime_type: mime_type.to_string(),
      text: None,
      encoding: None,
      file: None,
    },
  }
}

pub(crate) fn sha1_hex(bytes: &[u8]) -> String {
  use sha1::{Digest, Sha1};
  let digest = Sha1::digest(bytes);
  let mut out = String::with_capacity(40);
  for byte in digest {
    use std::fmt::Write;
    let _ = write!(out, "{byte:02x}");
  }
  out
}

/// File extension for an attached body, from its mime type. Mirrors the
/// `mime.getExtension(...) || 'dat'` fallback in `harTracer.ts:563` for
/// the types browsers commonly emit.
fn mime_extension(mime_type: &str) -> &'static str {
  let essence = mime_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
  match essence.as_str() {
    "text/html" => "html",
    "text/css" => "css",
    "text/plain" => "txt",
    "text/xml" | "application/xml" => "xml",
    "text/csv" => "csv",
    "text/markdown" => "md",
    "text/javascript" | "application/javascript" | "application/x-javascript" => "js",
    "application/json" => "json",
    "application/pdf" => "pdf",
    "application/zip" => "zip",
    "application/wasm" => "wasm",
    "image/png" => "png",
    "image/jpeg" => "jpeg",
    "image/gif" => "gif",
    "image/webp" => "webp",
    "image/svg+xml" => "svg",
    "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
    "image/avif" => "avif",
    "font/woff" | "application/font-woff" => "woff",
    "font/woff2" => "woff2",
    "font/ttf" => "ttf",
    "font/otf" => "otf",
    "audio/mpeg" => "mp3",
    "audio/wav" => "wav",
    "audio/ogg" => "ogg",
    "video/mp4" => "mp4",
    "video/webm" => "webm",
    _ => "dat",
  }
}

/// Decoded query pairs for a HAR entry's `queryString` (harTracer.ts
/// derives them from `new URL(request.url).searchParams`).
fn query_pairs(url: &str) -> Vec<HarHeaderOut> {
  let Some(query) = url.split_once('?').map(|(_, rest)| rest) else {
    return Vec::new();
  };
  let query = query.split('#').next().unwrap_or("");
  query
    .split('&')
    .filter(|pair| !pair.is_empty())
    .map(|pair| {
      let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
      HarHeaderOut {
        name: percent_decode(name),
        value: percent_decode(value),
      }
    })
    .collect()
}

/// Decode `%XX` escapes and `+`-as-space (application/x-www-form-urlencoded).
fn percent_decode(input: &str) -> String {
  let bytes = input.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    match bytes[i] {
      b'+' => {
        out.push(b' ');
        i += 1;
      },
      b'%' if i + 2 < bytes.len() => {
        let hex = |b: u8| (b as char).to_digit(16);
        if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
          #[allow(clippy::cast_possible_truncation)]
          out.push((hi * 16 + lo) as u8);
          i += 3;
        } else {
          out.push(bytes[i]);
          i += 1;
        }
      },
      other => {
        out.push(other);
        i += 1;
      },
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// Case-insensitive header lookup — CDP lowercases header names but
/// `WebKit` and `BiDi` deliver them as sent (`Content-Type`), and a HAR
/// recorded with `mimeType: x-unknown` replays as a download instead of
/// a rendered document.
fn header_value(headers: &crate::network::Headers, name: &str) -> Option<String> {
  headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case(name))
    .map(|(_, v)| v.clone())
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
  epoch_ms_to_iso8601(i64::try_from(now.as_millis()).unwrap_or(i64::MAX))
}

/// Format Unix-epoch milliseconds as ISO-8601 with millisecond
/// precision (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
fn epoch_ms_to_iso8601(total_ms: i64) -> String {
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
  let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
  let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
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
        file: None,
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
  /// `attach` policy: resource file name (`<sha1>.<ext>`) holding the
  /// body — a zip entry for `.zip` archives, a sibling file otherwise.
  #[serde(rename = "_file", skip_serializing_if = "Option::is_none")]
  file: Option<String>,
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
