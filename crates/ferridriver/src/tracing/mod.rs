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

mod har;

pub use har::{
  HarContentPolicy, HarMode, HarRecorder, StartHarOptions, epoch_ms_to_iso8601, now_iso8601, recorder_for_record_har,
  sha1_hex,
};
use har::{build_entry, is_zip_path};
pub(crate) use har::{collect_page_titles, flush_recorder};

use std::path::PathBuf;

use crate::context::ContextRef;
use crate::error::{FerriError, Result};
use crate::network::Request;
use crate::url_matcher::UrlMatcher;

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
      page_filter: None,
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
    let titles = collect_page_titles(&self.context_pages().await).await;
    flush_recorder(&recorder, &requests, &titles).await
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
  #[track_caller]
  pub fn start(&self) -> crate::action::Action<'_, crate::trace::TracingStartOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.start_impl(opts).await }))
  }

  async fn start_impl(&self, options: crate::trace::TracingStartOptions) -> Result<()> {
    let composite = self.ctx.composite();
    let (browser_name, context_options, traces_dir) = {
      let state = self.ctx.state().read().await;
      let browser_name = state.engine_name();
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
      (
        browser_name,
        serde_json::Value::Object(context_options),
        state.traces_dir_for(&composite),
      )
    };
    let network_len = self.network_log_len().await;
    // `browserType.launch({ tracesDir })` decides where the loose files
    // live; without one they go to a temp directory this recorder owns
    // and removes. A named recording under a caller's tracesDir is what
    // a viewer follows while the test is still running.
    let location = match traces_dir {
      Some(dir) => {
        crate::trace::TraceLocation::in_dir(dir, options.name.clone().unwrap_or_else(|| "trace".to_string()))
      },
      None => crate::trace::TraceLocation::temporary(),
    };
    let recorder = std::sync::Arc::new(crate::trace::TraceRecorder::new(
      &options,
      browser_name.to_string(),
      context_options,
      network_len,
      location,
    )?);
    crate::trace::install_recorder(&composite, std::sync::Arc::clone(&recorder))?;
    // Pages that predate the recorder still belong to the trace —
    // Playwright's Snapshotter walks `context.pages()` on start
    // (`packages/playwright-core/src/server/trace/recorder/snapshotter.ts`).
    // Without this, the viewer has actions whose `pageId` was never
    // opened, and every action a pre-existing page performs lands
    // outside any page tab.
    for page in self.context_pages().await {
      crate::trace::record_page_open(&recorder, &crate::trace::trace_page_id(&page));
    }
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

  /// Playwright: `tracing.startChunk(options?: { name?, title? })`.
  /// Resets the chunk-local event/resource buffers; the recorder keeps
  /// running. `name` renames the stream the chunk is written to, `title`
  /// relabels it in the viewer.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started.
  #[track_caller]
  pub fn start_chunk(&self) -> crate::action::Action<'_, crate::trace::TracingChunkOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.start_chunk_impl(opts).await }))
  }

  async fn start_chunk_impl(&self, options: crate::trace::TracingChunkOptions) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before starting a new chunk".to_string()))?;
    recorder.start_chunk(self.network_log_len().await, options.name, options.title);
    Ok(())
  }

  /// Playwright: `tracing.group(name, options?: { location? })`. Nests
  /// everything recorded until [`Self::group_end`] under one titled
  /// entry in the viewer's action tree.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started.
  pub fn group(&self, name: String, location: Option<crate::trace::StackFrame>) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before creating a group".to_string()))?;
    recorder.begin_group(name, location.into_iter().collect());
    Ok(())
  }

  /// Playwright: `tracing.groupEnd()`. Closes the innermost open group;
  /// closing when none is open is not an error, as in Playwright.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started.
  pub fn group_end(&self) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before ending a group".to_string()))?;
    recorder.end_group();
    Ok(())
  }

  /// Playwright: `tracing.stopChunk(options?: { path? })`. Exports the
  /// current chunk (when `path` is given) and starts a fresh one;
  /// tracing keeps running.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started or the export fails.
  #[track_caller]
  pub fn stop_chunk(&self) -> crate::action::Action<'_, crate::trace::TracingStopOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.stop_chunk_impl(opts).await }))
  }

  async fn stop_chunk_impl(&self, options: crate::trace::TracingStopOptions) -> Result<()> {
    let recorder = crate::trace::recorder_for(&self.ctx.composite())
      .ok_or_else(|| FerriError::backend("Must start tracing before stopping".to_string()))?;
    if let Some(path) = options.path {
      let network = self.trace_network_entries(&recorder).await;
      export_blocking(std::sync::Arc::clone(&recorder), path, network).await?;
    }
    recorder.start_chunk(self.network_log_len().await, None, None);
    Ok(())
  }

  /// Playwright: `tracing.stop(options?: { path? })`. Ends the
  /// recording, writing `trace.zip` when `path` is given.
  ///
  /// # Errors
  ///
  /// Errors if tracing was not started or the export fails.
  #[track_caller]
  pub fn stop(&self) -> crate::action::Action<'_, crate::trace::TracingStopOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.stop_impl(opts).await }))
  }

  async fn stop_impl(&self, options: crate::trace::TracingStopOptions) -> Result<()> {
    let composite = self.ctx.composite();
    let recorder = crate::trace::take_recorder(&composite)
      .ok_or_else(|| FerriError::backend("Must start tracing before stopping".to_string()))?;
    recorder.stop_screencasts().await;
    if let Some(path) = options.path {
      let network = self.trace_network_entries(&recorder).await;
      export_blocking(recorder, path, network).await?;
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
      page_filter: None,
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
        if let Some(content) = value.pointer_mut("/response/content").and_then(|c| c.as_object_mut())
          && let Some(file) = content.remove("_file")
        {
          content.insert("_sha1".to_string(), file);
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
          // Playwright's HAR extension for a request that never got a
          // response; without it a blocked or refused request reads as an
          // unexplained status 0.
          if let Some(failure) = req.failure() {
            obj.insert("_failureText".to_string(), serde_json::json!(failure));
          }
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
/// Run the final zip export on the blocking pool — deflating a whole
/// recording (screencast frames, snapshots) can take long enough to
/// starve the async worker the caller runs on.
async fn export_blocking(
  recorder: std::sync::Arc<crate::trace::TraceRecorder>,
  path: std::path::PathBuf,
  network: Vec<serde_json::Value>,
) -> Result<()> {
  tokio::task::spawn_blocking(move || recorder.export(&path, &network))
    .await
    .map_err(|e| FerriError::backend(format!("trace export task: {e}")))?
}
