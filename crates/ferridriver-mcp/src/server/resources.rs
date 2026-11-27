//! MCP resource listing/reading, artifact persistence, and prompt definitions for [`McpServer`].

use super::McpServer;
use base64::Engine;
use rmcp::{
  ErrorData,
  model::{
    ContentBlock, GetPromptRequestParams, GetPromptResult, Prompt, PromptArgument, PromptMessage,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, Role,
  },
};

impl McpServer {
  /// Build the JSON snapshot returned by the `network` MCP resource.
  /// Extracted from `read_resource` because async lock + per-request
  /// snapshotting pushed that handler over the line-count threshold.
  async fn read_network_resource(&self, context_name: &str, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let handles = self
      .state
      .log_handles_for(context_name)
      .await
      .ok_or_else(|| Self::err(format!("Context '{context_name}' not found")))?;
    let reqs = handles.network.read().await;
    let last: Vec<_> = reqs
      .iter()
      .rev()
      .take(100)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect();
    drop(reqs);
    let mut snapshots = Vec::with_capacity(last.len());
    for r in &last {
      snapshots.push(r.to_diagnostic_json().await);
    }
    let text = serde_json::to_string_pretty(&snapshots).unwrap_or_default();
    Ok(ReadResourceResult::new(vec![
      ResourceContents::text(text, uri.to_string()).with_mime_type("application/json"),
    ]))
  }

  /// Serve an `artifact://<relpath>` resource: raw bytes of a file the
  /// scripting layer wrote under `artifacts_root`. Binary payloads
  /// (screenshots, PDFs, traces) ship as base64 blobs; UTF-8 text ships as
  /// text. The artifacts rejects traversal / symlink escapes.
  async fn read_artifact_resource(&self, rel: &str, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let artifacts = self
      .artifacts_dir
      .as_ref()
      .ok_or_else(|| Self::err("artifacts are disabled: artifacts_root could not be prepared at startup"))?;
    let resolved = artifacts
      .resolve_read(rel)
      .map_err(|e| Self::err(format!("artifact path: {}", e.message)))?;
    if !resolved.starts_with(artifacts.root()) {
      return Err(Self::err("artifact path escapes the artifacts directory"));
    }
    let bytes = tokio::fs::read(&resolved)
      .await
      .map_err(|e| Self::err(format!("read artifact {rel}: {e}")))?;
    let mime = mime_for_path(rel);
    let contents = match String::from_utf8(bytes) {
      Ok(text) if mime.starts_with("text/") || mime == "application/json" => {
        ResourceContents::text(text, uri.to_string()).with_mime_type(mime)
      },
      Ok(text) => {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        ResourceContents::blob(b64, uri.to_string()).with_mime_type(mime)
      },
      Err(e) => {
        let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
        ResourceContents::blob(b64, uri.to_string()).with_mime_type(mime)
      },
    };
    Ok(ReadResourceResult::new(vec![contents]))
  }

  /// Write `bytes` to `rel` under `artifacts_root` and return an
  /// `artifact://<rel>` resource link for it. `None` when artifacts are
  /// disabled or the write fails — artifact persistence is best-effort and
  /// never fails the caller's primary result.
  pub(crate) async fn persist_artifact(&self, rel: &str, bytes: &[u8], mime: &str) -> Option<ContentBlock> {
    let artifacts = self.artifacts_dir.as_ref()?;
    let resolved = artifacts.resolve_write(rel).ok()?;
    if let Some(parent) = resolved.parent() {
      tokio::fs::create_dir_all(parent).await.ok()?;
    }
    tokio::fs::write(&resolved, bytes).await.ok()?;
    // Sweep after the write, protecting exactly the file just written: the
    // link handed back below must still resolve when the caller follows it.
    if let Some(budget) = self.artifacts_budget {
      let keep = std::collections::BTreeSet::from([resolved.clone()]);
      let evicted = budget.enforce(artifacts.root(), &keep).await;
      if evicted.files > 0 {
        tracing::info!(
          files = evicted.files,
          bytes = evicted.bytes,
          "artifacts budget: evicted least-recently-modified outputs"
        );
      }
    }
    let uri = format!("artifact://{rel}");
    let link = Resource::new(uri, rel)
      .with_mime_type(mime.to_string())
      .with_size(bytes.len() as u64);
    Some(ContentBlock::ResourceLink(link))
  }

  /// Enumerate files under `artifacts_root` (bounded) as `artifact://`
  /// resources for `resources/list`. Skips directories; caps the count so a
  /// runaway output dir can't blow up the listing. Entries are ordered
  /// newest-first before the cap is applied — `read_dir` order is
  /// arbitrary, and truncating an arbitrary order silently drops the
  /// artifacts a client just created once the directory outgrows the cap.
  pub(super) async fn list_artifact_resources(&self, out: &mut Vec<Resource>) {
    const MAX: usize = 200;
    /// Scan bound: enough to sort recency over years of accumulated
    /// outputs without letting a runaway directory stall the listing.
    const SCAN_MAX: usize = 4096;
    let Some(artifacts) = self.artifacts_dir.as_ref() else {
      return;
    };
    let root = artifacts.root().to_path_buf();
    let mut found: Vec<(std::time::SystemTime, Resource)> = Vec::new();
    let mut stack = vec![root.clone()];
    'walk: while let Some(dir) = stack.pop() {
      let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        continue;
      };
      while let Ok(Some(entry)) = entries.next_entry().await {
        if found.len() >= SCAN_MAX {
          break 'walk;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type().await else { continue };
        if ft.is_dir() {
          stack.push(path);
          continue;
        }
        let Ok(rel) = path.strip_prefix(&root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let meta = entry.metadata().await.ok();
        let modified = meta
          .as_ref()
          .and_then(|m| m.modified().ok())
          .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let mut r = Resource::new(format!("artifact://{rel}"), rel.clone()).with_mime_type(mime_for_path(&rel));
        if let Some(size) = meta.map(|m| m.len()) {
          r = r.with_size(size);
        }
        found.push((modified, r));
      }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    out.extend(found.into_iter().take(MAX).map(|(_, r)| r));
  }

  /// The body of `ServerHandler::read_resource`, kept as an inherent method
  /// so every arm can build a plain [`ReadResourceResult`]; the trait boundary
  /// widens it to the MRTR response enum once.
  pub(super) async fn read_resource_contents(
    &self,
    request: ReadResourceRequestParams,
  ) -> Result<ReadResourceResult, ErrorData> {
    let uri = &request.uri;
    if let Some(rel) = uri.strip_prefix("artifact://") {
      return self.read_artifact_resource(rel, uri).await;
    }
    let (context_name, resource) = if let Some(rest) = uri.strip_prefix("browser://session/") {
      let mut parts = rest.splitn(2, '/');
      (
        parts.next().unwrap_or("default").to_string(),
        parts.next().unwrap_or("").to_string(),
      )
    } else if let Some(stripped) = uri.strip_prefix("browser://") {
      ("default".to_string(), stripped.to_string())
    } else {
      return Err(Self::err(format!("Unknown resource URI: {uri}")));
    };

    let page = Box::pin(self.page(&context_name)).await?;

    match resource.as_str() {
      "page-info" => {
        let url = page.url();
        let title = page.title().await.unwrap_or_default();
        let json =
          serde_json::to_string_pretty(&serde_json::json!({"url": url, "title": title, "session": context_name}))
            .unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(json, uri).with_mime_type("application/json"),
        ]))
      },
      "console" => {
        let handles = self
          .state
          .log_handles_for(&context_name)
          .await
          .ok_or_else(|| Self::err(format!("Context '{context_name}' not found")))?;
        let msgs = handles.console.read().await;
        let last: Vec<serde_json::Value> = msgs
          .iter()
          .rev()
          .take(100)
          .map(|m| {
            serde_json::json!({
              "type": m.type_str(),
              "text": m.text(),
            })
          })
          .collect::<Vec<_>>()
          .into_iter()
          .rev()
          .collect();
        drop(msgs);
        let text = serde_json::to_string_pretty(&last).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      "network" => self.read_network_resource(&context_name, uri).await,
      "snapshot" => {
        let snap = self.snap(&page, &context_name).await;
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(snap, uri).with_mime_type("text/plain"),
        ]))
      },
      "screenshot" => {
        let bytes = page.screenshot().await.map_err(Self::err)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(ReadResourceResult::new(vec![
          ResourceContents::blob(b64, uri).with_mime_type("image/png"),
        ]))
      },
      "cookies" => {
        let cookies = page.inner().get_cookies().await.map_err(Self::err)?;
        let list: Vec<serde_json::Value> = cookies
          .iter()
          .map(|c| serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain}))
          .collect();
        let text = serde_json::to_string_pretty(&list).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      _ => Err(Self::err(format!("Unknown resource: {uri}"))),
    }
  }

  /// The prompts this server advertises. Compiled in, so the listing is the
  /// same on every call.
  pub(super) fn prompt_definitions() -> Vec<Prompt> {
    vec![
      Prompt::new(
        "debug-page",
        Some("Analyze the page for errors, broken elements, and console issues"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to debug")
            .with_required(false),
        ]),
      ),
      Prompt::new(
        "test-form",
        Some("Fill and submit a form, verify the result"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("Page URL with the form")
            .with_required(true),
          PromptArgument::new("submit_selector")
            .with_description("Submit button selector")
            .with_required(false),
        ]),
      ),
      Prompt::new(
        "audit-accessibility",
        Some("Check page accessibility using the a11y tree"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to audit")
            .with_required(true),
        ]),
      ),
      Prompt::new(
        "compare-sessions",
        Some("Compare page state between two browser sessions"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to compare")
            .with_required(true),
          PromptArgument::new("session_a")
            .with_description("First session")
            .with_required(true),
          PromptArgument::new("session_b")
            .with_description("Second session")
            .with_required(true),
        ]),
      ),
    ]
  }

  /// The body of `ServerHandler::get_prompt`: builds the messages for one
  /// named prompt, which the trait boundary widens to the MRTR response enum.
  pub(super) fn prompt_messages(request: &GetPromptRequestParams) -> Result<GetPromptResult, ErrorData> {
    let args = request.arguments.clone().unwrap_or_default();
    let get_arg = |key: &str| -> String { args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() };
    let url = get_arg("url");

    match request.name.as_str() {
      "debug-page" => {
        let nav = if url.is_empty() {
          String::new()
        } else {
          format!("First navigate to {url}.\n")
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          Role::User,
          format!(
            "{nav}Debug the current page:\n1. Take a snapshot to understand the page structure\n2. Check console_messages for errors\n3. Check network_requests for failed requests (4xx/5xx)\n4. Report all issues found with suggested fixes"
          ),
        )]))
      },
      "test-form" => {
        let submit = {
          let s = get_arg("submit_selector");
          if s.is_empty() { "the submit button".into() } else { s }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          Role::User,
          format!(
            "Test the form on {url}:\n1. Navigate to the page\n2. Take a snapshot to identify form fields\n3. Fill all required fields with realistic test data\n4. Click {submit}\n5. Verify the form submitted successfully\n6. Report the result"
          ),
        )]))
      },
      "audit-accessibility" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
        Role::User,
        format!(
          "Audit the accessibility of {url}:\n1. Navigate to the page\n2. Take a snapshot (a11y tree)\n3. Check for: missing labels, incorrect heading hierarchy, images without alt text, interactive elements without accessible names, form inputs without labels\n4. Report issues with severity and how to fix each one"
        ),
      )])),
      "compare-sessions" => {
        let sa = {
          let s = get_arg("session_a");
          if s.is_empty() { "userA".into() } else { s }
        };
        let sb = {
          let s = get_arg("session_b");
          if s.is_empty() { "userB".into() } else { s }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          Role::User,
          format!(
            "Compare {url} between two sessions:\n1. Open the page in session='{sa}' and session='{sb}'\n2. Take a snapshot of each\n3. Compare: visible content differences, available navigation, form fields, cookies\n4. Report what differs between the two sessions"
          ),
        )]))
      },
      _ => Err(Self::err(format!("Unknown prompt: {}", request.name))),
    }
  }
}

/// Best-effort MIME type from a file extension, for `artifact://` resources.
/// Covers the formats the scripting layer actually emits (screenshots, PDFs,
/// traces, text); everything else falls back to `application/octet-stream`.
fn mime_for_path(path: &str) -> String {
  let ext = std::path::Path::new(path)
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or("")
    .to_ascii_lowercase();
  match ext.as_str() {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "webp" => "image/webp",
    "gif" => "image/gif",
    "svg" => "image/svg+xml",
    "pdf" => "application/pdf",
    "json" | "trace" => "application/json",
    "txt" | "log" => "text/plain",
    "html" | "htm" => "text/html",
    "csv" => "text/csv",
    "zip" => "application/zip",
    _ => "application/octet-stream",
  }
  .to_string()
}
