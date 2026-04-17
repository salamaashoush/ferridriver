//! Scripting tool — run sandboxed `QuickJS` against the live session.
//!
//! Each invocation gets a fresh `rquickjs` context (no state bleeds between
//! calls). `vars` persists per session so scripts can share computed values
//! across invocations. `fs` is scoped to the configured `script_root`.
//!
//! Args are passed as a positional array bound to the `args` global; they
//! are never interpolated into the source string, which prevents prompt-
//! injection paths where a malicious arg value becomes executable code.

use std::time::Duration;

use ferridriver_script::{RunContext, RunOptions};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
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
    `fs` (readFile/readFileBytes/writeFile/readdir/exists — scoped to the configured script_root), \
    `page` / `context` / `request` (live browser bindings). \
    Do NOT interpolate caller-controlled data into this string; pass it via `args` instead."
  )]
  pub source: Option<String>,

  #[schemars(
    description = "Path to a `.js` or `.mjs` file to execute, relative to the configured \
    script_root. Mutually exclusive with `source`. Lets the LLM iterate on a saved script by \
    editing the file and re-invoking `run_script` without re-sending the full source each call. \
    The path is validated against script_root: absolute paths, `..` components, and symlinks \
    escaping the root are rejected. Error line numbers in the script result are file-relative."
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
}

// ── Tool implementation ─────────────────────────────────────────────────────

#[tool_router(router = script_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "run_script",
    description = "Execute JavaScript in a sandboxed QuickJS runtime against the current session. \
    Provide `source` (inline JS) or `path` (a .js/.mjs file under script_root) — exactly one. \
    Use `path` to iterate on a saved script: edit the file, re-invoke, no need to resend the body. \
    Use this for imperative browser-automation logic that needs loops, conditionals, try/catch, \
    or computed values. \
    Globals available: `args` (bound parameters, never interpolated into source — prompt-injection safe), \
    `vars` (session-level get/set/has/delete), `console.*` (captured with limits), \
    `fs` (readFile/writeFile/readdir/exists, scoped to script_root, rejects path traversal), \
    `page` / `context` / `request` (live browser bindings). \
    Fresh QuickJS context per call — no state bleeds between invocations except through `vars`. \
    Returns structured JSON: { status: 'ok'|'error', value?, error?, duration_ms, console[] }. \
    On `error`, the payload includes message, stack, line, column, and a source snippet around the failure. \
    Pair with snapshot/screenshot tools when the LLM needs to ground selectors before acting."
  )]
  async fn run_script(&self, Parameters(p): Parameters<RunScriptParams>) -> Result<CallToolResult, ErrorData> {
    let session = sess(p.session.as_ref()).to_string();

    let Some(sandbox) = self.script_sandbox.clone() else {
      return Err(McpServer::err(
        "scripting is disabled: the configured script_root could not be prepared at server startup. \
        Check the server log for the underlying error.",
      ));
    };

    let vars = self.session_vars(&session);

    let options = RunOptions {
      timeout: p.timeout_ms.map(Duration::from_millis),
      memory_limit: p.memory_limit_mb.and_then(|mb| usize::try_from(mb * 1024 * 1024).ok()),
      stack_size: None,
    };

    let args = p.args.unwrap_or_default();

    // Resolve the script source from either `source` (inline) or `path`
    // (file under script_root). Exactly one must be provided.
    let source = match (p.source.as_deref(), p.path.as_deref()) {
      (Some(_), Some(_)) => {
        return Err(McpServer::err("run_script accepts `source` OR `path`, not both"));
      },
      (None, None) => {
        return Err(McpServer::err(
          "run_script requires either `source` (inline JS) or `path` (file under script_root)",
        ));
      },
      (Some(src), None) => src.to_string(),
      (None, Some(rel)) => {
        let resolved = sandbox
          .resolve_read(rel)
          .map_err(|e| McpServer::err(format!("run_script path: {}", e.message)))?;
        match resolved.extension().and_then(|e| e.to_str()) {
          Some("js" | "mjs") => {},
          _ => {
            return Err(McpServer::err(
              "run_script `path` must point at a .js or .mjs file under script_root",
            ));
          },
        }
        std::fs::read_to_string(&resolved)
          .map_err(|e| McpServer::err(format!("run_script read {}: {e}", resolved.display())))?
      },
    };

    // Resolve the session's active page so scripts can call `page.click/etc`.
    // We launch/attach eagerly (same as other tools) — pure-compute scripts
    // that don't touch `page` still work; they just pay the launch cost
    // once for the session.
    let (page, ctx_ref) = Box::pin(self.page_and_context(&session)).await?;
    let request = std::sync::Arc::new(ferridriver::api_request::APIRequestContext::new(
      ferridriver::api_request::RequestContextOptions::default(),
    ));

    let context = RunContext {
      vars,
      sandbox,
      page: Some(page),
      browser_context: Some(std::sync::Arc::new(ctx_ref)),
      request: Some(request),
    };

    let result = self.script_engine.run(&source, &args, options, context).await;

    let json = serde_json::to_string_pretty(&result).map_err(|e| McpServer::err(format!("serialize result: {e}")))?;

    // Build the return: one JSON text block is the mechanical payload the
    // caller (often an LLM) parses. Well-formed per ScriptResult's schema.
    let mut contents = vec![Content::text(json)];

    // On error, also surface a short human-readable summary so LLMs that skim
    // tool output see the failure reason without parsing JSON.
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      let summary = format!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
      contents.insert(0, Content::text(summary));
    }

    // We always return success at the MCP layer and let the caller inspect
    // `status` in the payload; a thrown script error is not an MCP error.
    Ok(CallToolResult::success(contents))
  }
}
