//! Scripting tool — run `QuickJS` against the live session.
//!
//! Each invocation gets a fresh `rquickjs` context (no state bleeds between
//! calls). `vars` persists per session so scripts can share computed values
//! across invocations. `fs` is Node's, and a relative `path` resolves
//! against the configured `script_root`.
//!
//! Args are passed as a positional array bound to the `args` global; they
//! are never interpolated into the source string, which prevents prompt-
//! injection paths where a malicious arg value becomes executable code.

use std::time::Duration;

use ferridriver_script::RunOptions;
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, ContentBlock},
  tool, tool_router,
};
use serde::Deserialize;

use crate::server::{McpServer, sess};

// ── Parameter type ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunScriptParams {
  #[schemars(
    description = "Inline JavaScript source to execute. Mutually exclusive with `path`. \
    Runs inside an async IIFE so top-level `await` works; use `return <value>` to return a result. \
    The script has access to these globals: \
    `args` (array of bound parameters), \
    `vars` (session-level string store: get/set/has/delete/keys), \
    `console` (log/info/warn/error/debug — captured and returned), \
    `fs` (Node's `node:fs`: readFileSync/writeFileSync/readdirSync/existsSync plus `promises`), \
    `artifacts` (write/writeBytes/read/readBytes/list/exists/remove — dedicated output dir for \
    screenshots, PDFs, traces; scoped to the configured artifacts_root), \
    `page` / `context` / `request` (live browser bindings). \
    Do NOT interpolate caller-controlled data into this string; pass it via `args` instead."
  )]
  pub source: Option<String>,

  #[schemars(
    description = "Path to a script file, relative to the configured script_root. Accepts \
    `.js`/`.mjs` and `.ts`/`.tsx`/`.mts`/`.cts`. Mutually exclusive with `source`. Lets the LLM \
    iterate on a saved script by editing the file and re-invoking `run_script` without re-sending \
    the full source. A TypeScript file, or any file with top-level `import`/`export`, is bundled \
    + transpiled and run as an ES module: top-level `await` works and the result is the module's \
    `default` export (use `export default <value>`); a plain `.js` file keeps the `return <value>` \
    convention. Imports are resolved off disk. The path itself resolves against script_root: \
    absolute paths, `..` components, and symlinks escaping the root are rejected. Error line \
    numbers are remapped to the original source."
  )]
  pub path: Option<String>,

  #[schemars(
    description = "Positional arguments made available inside the script as the `args` array. \
    Values are bound, never interpolated into `source` — safe to contain arbitrary strings, \
    objects, or arrays. Access with `args[0]`, `args[1]`, etc. Default: empty array."
  )]
  pub args: Option<Vec<serde_json::Value>>,

  #[schemars(description = "Override the per-script wall-clock timeout, in milliseconds. \
    Default is set by the server config (5 minutes). Cannot exceed the configured maximum.")]
  pub timeout_ms: Option<u64>,

  #[schemars(description = "Override the per-script memory quota, in megabytes. \
    Default is set by the server config (256 MiB).")]
  pub memory_limit_mb: Option<u64>,

  #[schemars(
    description = "Session identifier (same format as other tools: 'instance:context'). \
    Session-scoped `vars` persist across `run_script` calls with the same session. \
    Default: 'default'."
  )]
  pub session: Option<String>,

  #[schemars(
    description = "Also return the source that reproduces every browser action this script \
    performed, in `ts` (Playwright-shaped TypeScript), `rust` (a #[ferritest] body), or \
    `gherkin` (feature steps). Use it to turn an exploratory run into a test: drive the app \
    with run_script, keep the `code` array it returns. Omit to skip the work entirely."
  )]
  pub code_language: Option<String>,
}

/// `run_script`'s reply: the engine's result, plus the generated source when
/// the caller asked for it.
///
/// A dedicated type (rather than adding a field to `ScriptResult`) keeps the
/// engine's own result shape untouched while still letting the tool declare an
/// accurate `outputSchema`.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct RunScriptOutput {
  #[serde(flatten)]
  pub result: ferridriver_script::ScriptResult,
  /// One line per browser action, in call order. Empty unless
  /// `code_language` was set.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub code: Vec<String>,
  /// The page the session is left on: its URL, title, and how many console
  /// errors / warnings / uncaught exceptions the current document produced.
  /// Absent only if the page could not be read.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub page: Option<PageStateOut>,
}

/// [`ferridriver::response::PageState`] with a schema, so the tool's declared
/// `outputSchema` describes the page section instead of an opaque object.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageStateOut {
  pub url: String,
  pub title: String,
  pub console_errors: usize,
  pub console_warnings: usize,
  pub page_errors: usize,
}

impl From<ferridriver::response::PageState> for PageStateOut {
  fn from(state: ferridriver::response::PageState) -> Self {
    Self {
      url: state.url,
      title: state.title,
      console_errors: state.console_errors,
      console_warnings: state.console_warnings,
      page_errors: state.page_errors,
    }
  }
}

// ── Response sections ───────────────────────────────────────────────────────

impl McpServer {
  /// Render a finished `run_script` as the titled sections an agent reads.
  ///
  /// The JSON payload stays the machine contract; this is the same run said
  /// once, briefly, in the order the reader needs it — what failed, what came
  /// back, what reproduces it, where the browser now is.
  fn run_script_sections(
    &self,
    result: &ferridriver_script::ScriptResult,
    code: &[String],
    code_language: Option<&str>,
    page: Option<&ferridriver::response::PageState>,
  ) -> String {
    let mut response = ferridriver::response::Response::new().with_secrets(self.secrets.clone());
    match &result.outcome {
      ferridriver_script::Outcome::Error { error } => {
        let name = error.name.clone().unwrap_or_else(|| error.kind.to_string());
        response.error(vec![format!("{name}: {}", error.message)]);
      },
      ferridriver_script::Outcome::Ok { success } => match &success.value {
        serde_json::Value::Null => {},
        serde_json::Value::String(s) => response.result(s.lines().map(str::to_string).collect()),
        value => response.result(
          serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| value.to_string())
            .lines()
            .map(str::to_string)
            .collect(),
        ),
      },
    }
    if let Some(language) = code_language {
      response.code(code.to_vec(), ferridriver::codegen::OutputLanguage::parse_cli(language));
    }
    if let Some(page) = page {
      response.page(page);
    }
    response.render()
  }
}

// ── Tool implementation ─────────────────────────────────────────────────────

#[tool_router(router = script_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "run_script",
    title = "Run Browser Script",
    annotations(read_only_hint = false, open_world_hint = true),
    description = "Execute JavaScript in a QuickJS runtime against the current session. \
    Provide `source` (inline JS) or `path` (a .js/.mjs file) — exactly one. \
    Use `path` to iterate on a saved script: edit the file, re-invoke, no need to resend the body. \
    Use this for imperative browser-automation logic that needs loops, conditionals, try/catch, \
    or computed values. \
    Globals available: `args` (bound parameters, never interpolated into source — prompt-injection safe), \
    `vars` (session-level get/set/has/delete), `console.*` (captured with limits), \
    `fs` (Node's `node:fs`, including `fs.promises`), \
    `artifacts` (write/writeBytes/read/readBytes/list/exists/remove, dedicated output dir for \
    screenshots / PDFs / traces; pair with `page.screenshot()` or `page.pdf()` to save bytes), \
    `page` / `context` / `request` (live browser bindings). \
    The session VM persists between calls (same `globalThis` + `vars`), but `page.on(...)` \
    listeners only execute while a script is actively running — events arriving between calls \
    buffer (bounded; oldest kept) and deliver at the start of the next call. For reliable \
    cross-call observation poll `page.consoleMessages()` / `page.pageErrors()` (retained \
    history) or use `page.waitForEvent(event, { predicate })` inside one script. \
    Returns structured JSON: { status: 'ok'|'error', value?, error?, duration_ms, console[], code[]? }. \
    On `error`, the payload includes message, stack, line, column, and a source snippet around the failure. \
    Set `code_language` to also get back the source reproducing every action the script performed \
    (TypeScript, Rust, or Gherkin) — that is how an exploratory run becomes a committed test. \
    Pair with snapshot/screenshot tools when the LLM needs to ground selectors before acting.",
    output_schema = rmcp::handler::server::tool::schema_for_output::<RunScriptOutput>()
  )]
  async fn run_script(
    &self,
    Parameters(p): Parameters<RunScriptParams>,
    meta: rmcp::model::RequestMetaObject,
    peer: rmcp::service::Peer<rmcp::RoleServer>,
  ) -> Result<CallToolResult, ErrorData> {
    let session = sess(p.session.as_ref()).to_string();
    let token = meta.get_progress_token();
    // Serialize per-session: a concurrent run_script / extension / navigation
    // call on the same session must not interleave browser state.
    let guard = self.session_guard(&session).await;
    McpServer::emit_progress(&peer, token.as_ref(), 0.0, Some(1.0), "executing script").await;

    let Some(script_root) = self.script_root.clone() else {
      return Err(McpServer::err(
        "scripting is disabled: the configured script_root could not be prepared at server startup. \
        Check the server log for the underlying error.",
      ));
    };

    let options = RunOptions {
      timeout: p.timeout_ms.map(Duration::from_millis),
      memory_limit: p.memory_limit_mb.and_then(|mb| usize::try_from(mb * 1024 * 1024).ok()),
      stack_size: None,
      gc_threshold: None,
    };

    let args = p.args.unwrap_or_default();

    // Which artifacts already existed, so the sweep below can tell this
    // call's outputs from every earlier call's. The sandbox outlives the
    // call, so its whole record is not "what this call wrote".
    let artifacts_before = self
      .artifacts_dir
      .as_ref()
      .map(|sandbox| sandbox.written())
      .unwrap_or_default();

    // Resolve the script source from either `source` (inline) or `path`
    // (file under script_root). Exactly one must be provided. A file
    // entry is also remembered so an ES-module source (TypeScript, or
    // `import`/`export`) can be bundled and run as a module.
    let (source, module_entry): (String, Option<std::path::PathBuf>) = match (p.source.as_deref(), p.path.as_deref()) {
      (Some(_), Some(_)) => {
        return Err(McpServer::err("run_script accepts `source` OR `path`, not both"));
      },
      (None, None) => {
        return Err(McpServer::err(
          "run_script requires either `source` (inline JS) or `path` (a JS/TS file)",
        ));
      },
      // Inline source stays on the raw wrap-and-eval path (top-level
      // `return` yields the result); only file paths are bundled.
      (Some(src), None) => (src.to_string(), None),
      (None, Some(rel)) => {
        // Resolved the way `fs` resolves: relative to the script root,
        // absolute as written.
        let joined = if std::path::Path::new(rel).is_absolute() {
          std::path::PathBuf::from(rel)
        } else {
          script_root.join(rel)
        };
        let resolved = std::fs::canonicalize(&joined)
          .map_err(|e| McpServer::err(format!("run_script path {}: {e}", joined.display())))?;
        match resolved.extension().and_then(|e| e.to_str()) {
          Some("js" | "mjs" | "ts" | "tsx" | "mts" | "cts") => {},
          _ => {
            return Err(McpServer::err(
              "run_script `path` must point at a .js/.mjs/.ts/.tsx/.mts/.cts file",
            ));
          },
        }
        let src = std::fs::read_to_string(&resolved)
          .map_err(|e| McpServer::err(format!("run_script read {}: {e}", resolved.display())))?;
        let entry = (ferridriver_script::is_typescript_path(&resolved)
          || ferridriver_script::source_is_es_module(&src))
        .then_some(resolved);
        (src, entry)
      },
    };

    // Live page/context/request/browser handles + sandboxes + extensions.
    // `mcp_run_context` launches/attaches the session's browser eagerly
    // (pure-compute scripts still work; they just pay the one-time cost).
    let context = self.mcp_run_context(&session).await?;

    // Code echo, scoped to this session's composite key — the one every
    // action carries — so concurrent sessions never collect each other's
    // lines. Nothing is installed unless the caller asked.
    let collected_code = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let echo_guard = match p.code_language.as_deref() {
      None => None,
      Some(language) => {
        let language = ferridriver::codegen::OutputLanguage::parse_cli(language);
        let sink = std::sync::Arc::clone(&collected_code);
        Some(ferridriver::trace::observe_session_actions(
          &self.state.session_key(&session).await.to_composite(),
          std::sync::Arc::new(
            ferridriver::codegen::echo::CodeEcho::new(language, move |line| {
              if let Ok(mut lines) = sink.lock() {
                lines.push(line);
              }
            })
            .with_secrets(self.secrets.clone()),
          ),
        ))
      },
    };

    let result = if let Some(entry) = module_entry {
      // ES-module file: rolldown-bundle (TypeScript + imports, disk-cached)
      // and run as a module — the result is its `default` export.
      let bundle_cwd = entry
        .parent()
        .map_or_else(|| script_root.clone(), std::path::Path::to_path_buf);
      let bundle = ferridriver_script::bundle_and_compile(std::slice::from_ref(&entry), &bundle_cwd)
        .await
        .map_err(|e| McpServer::err(format!("run_script bundle {}: {}", entry.display(), e.message)))?;
      self
        .run_module_on_session_vm(&session, &guard, &bundle, &args, options, context)
        .await
    } else {
      self
        .run_on_session_vm(&session, &guard, &source, &args, options, context)
        .await
    };

    // Dropping the guard here (rather than at end of scope) unregisters the
    // observer before the reply is built, so no late action can append to a
    // vector that has already been read.
    drop(echo_guard);

    // Bring the artifacts root back under its ceiling, keeping whatever this
    // call produced — a script that writes a report and then has it deleted
    // on the way out has done nothing.
    if let (Some(budget), Some(sandbox)) = (self.artifacts_budget, self.artifacts_dir.as_ref()) {
      let mine: std::collections::BTreeSet<_> = sandbox
        .written()
        .into_iter()
        .filter(|path| !artifacts_before.contains(path))
        .collect();
      let evicted = budget.enforce(sandbox.root(), &mine).await;
      if evicted.files > 0 {
        tracing::info!(
          files = evicted.files,
          bytes = evicted.bytes,
          "artifacts budget: evicted least-recently-modified outputs"
        );
      }
    }

    // Where the script left the session. Captured after the run and before
    // the guard is released, so it describes the page the caller's NEXT call
    // will act on. Reading it opens no action span, so it never appears in
    // the code the run just echoed.
    let page_state = match Box::pin(self.page(&session)).await {
      Ok(page) => Some(ferridriver::response::PageState::capture(&page).await),
      Err(e) => {
        tracing::debug!(error = ?e, "page state unavailable for the response");
        None
      },
    };

    let code = collected_code
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    let output = RunScriptOutput {
      result,
      code: code.clone(),
      page: page_state.clone().map(PageStateOut::from),
    };
    let result = &output.result;

    let mut value = serde_json::to_value(&output).map_err(|e| McpServer::err(format!("serialize result: {e}")))?;
    // The engine already redacted the console, the value and the error; this
    // covers what the tool added around them (the page URL and title).
    self.secrets.redact_json(&mut value);
    let json = serde_json::to_string_pretty(&value).map_err(|e| McpServer::err(format!("serialize result: {e}")))?;

    // Build the return: one JSON text block is the mechanical payload the
    // caller (often an LLM) parses. Well-formed per RunScriptOutput's schema.
    let mut contents = vec![ContentBlock::text(json)];

    // Ahead of it, the same run as titled sections — the shape an agent reads
    // without parsing. Cheap to skip past, and it is what carries the page
    // the session is now on into a skimmed reading.
    let sections = self.run_script_sections(result, &code, p.code_language.as_deref(), page_state.as_ref());
    if !sections.is_empty() {
      contents.insert(0, ContentBlock::text(sections));
    }

    // On error, also surface a short human-readable summary so LLMs that skim
    // tool output see the failure reason without parsing JSON.
    let failed = matches!(result.outcome, ferridriver_script::Outcome::Error { .. });
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      let summary = format!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
      contents.insert(0, ContentBlock::text(summary));
    }

    McpServer::emit_progress(&peer, token.as_ref(), 1.0, Some(1.0), "done").await;

    // A script that threw is a tool execution error: `isError` is what a
    // client checks to know the call failed. The payload is unchanged —
    // `status` still carries the same detail — so callers that parse it
    // keep working and callers that only check `isError` start working.
    // Either way it also travels as structured content, which a client
    // validates against the tool's declared `outputSchema`.
    let mut out = if failed {
      CallToolResult::error(contents)
    } else {
      CallToolResult::success(contents)
    };
    out.structured_content = Some(value);
    Ok(out)
  }
}
