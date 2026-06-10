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

use ferridriver_script::RunOptions;
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
    `artifacts` (write/writeBytes/read/readBytes/list/exists/remove — dedicated output dir for \
    screenshots, PDFs, traces; scoped to the configured artifacts_root), \
    `page` / `context` / `request` (live browser bindings). \
    Do NOT interpolate caller-controlled data into this string; pass it via `args` instead."
  )]
  pub source: Option<String>,

  #[schemars(
    description = "Path to a script file under the configured script_root, relative. Accepts \
    `.js`/`.mjs` and `.ts`/`.tsx`/`.mts`/`.cts`. Mutually exclusive with `source`. Lets the LLM \
    iterate on a saved script by editing the file and re-invoking `run_script` without re-sending \
    the full source. A TypeScript file, or any file with top-level `import`/`export`, is bundled \
    + transpiled and run as an ES module: top-level `await` works and the result is the module's \
    `default` export (use `export default <value>`); a plain `.js` file keeps the `return <value>` \
    convention. Imports are resolved off disk but every resolved file must stay under script_root \
    (a traversal/symlink escape is rejected). The path itself is validated against script_root: \
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
    `artifacts` (write/writeBytes/read/readBytes/list/exists/remove, dedicated output dir for \
    screenshots / PDFs / traces; pair with `page.screenshot()` or `page.pdf()` to save bytes), \
    `page` / `context` / `request` (live browser bindings). \
    The session VM persists between calls (same `globalThis` + `vars`), but `page.on(...)` \
    listeners only execute while a script is actively running — events arriving between calls \
    buffer (bounded; oldest kept) and deliver at the start of the next call. For reliable \
    cross-call observation poll `page.consoleMessages()` / `page.pageErrors()` (retained \
    history) or use `page.waitForEvent(event, { predicate })` inside one script. \
    Returns structured JSON: { status: 'ok'|'error', value?, error?, duration_ms, console[] }. \
    On `error`, the payload includes message, stack, line, column, and a source snippet around the failure. \
    Pair with snapshot/screenshot tools when the LLM needs to ground selectors before acting."
  )]
  async fn run_script(&self, Parameters(p): Parameters<RunScriptParams>) -> Result<CallToolResult, ErrorData> {
    let session = sess(p.session.as_ref()).to_string();
    // Serialize per-session: a concurrent run_script / plugin / navigation
    // call on the same session must not interleave browser state.
    let guard = self.session_guard(&session).await;

    let Some(sandbox) = self.script_sandbox.clone() else {
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
          "run_script requires either `source` (inline JS) or `path` (file under script_root)",
        ));
      },
      // Inline source stays on the raw wrap-and-eval path (top-level
      // `return` yields the result); only file paths are bundled.
      (Some(src), None) => (src.to_string(), None),
      (None, Some(rel)) => {
        let resolved = sandbox
          .resolve_read(rel)
          .map_err(|e| McpServer::err(format!("run_script path: {}", e.message)))?;
        match resolved.extension().and_then(|e| e.to_str()) {
          Some("js" | "mjs" | "ts" | "tsx" | "mts" | "cts") => {},
          _ => {
            return Err(McpServer::err(
              "run_script `path` must point at a .js/.mjs/.ts/.tsx/.mts/.cts file under script_root",
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

    // Live page/context/request/browser handles + sandboxes + plugins.
    // `mcp_run_context` launches/attaches the session's browser eagerly
    // (pure-compute scripts still work; they just pay the one-time cost).
    let context = self.mcp_run_context(&session).await?;

    let result = if let Some(entry) = module_entry {
      // ES-module file: rolldown-bundle (TypeScript + imports, disk-cached)
      // and run as a module — the result is its `default` export. Bundling
      // resolves imports off disk, so every transitive input must be
      // re-validated under script_root before execution; an import that
      // escapes the sandbox is rejected (never run).
      let bundle_cwd = entry
        .parent()
        .map_or_else(|| sandbox.root().to_path_buf(), std::path::Path::to_path_buf);
      let bundle = ferridriver_script::bundle_and_compile(std::slice::from_ref(&entry), &bundle_cwd)
        .await
        .map_err(|e| McpServer::err(format!("run_script bundle {}: {}", entry.display(), e.message)))?;
      for input in bundle.source_files(&bundle_cwd) {
        let canon = std::fs::canonicalize(&input)
          .map_err(|e| McpServer::err(format!("run_script input {}: {e}", input.display())))?;
        if !canon.starts_with(sandbox.root()) {
          return Err(McpServer::err(format!(
            "run_script: import escapes script_root: {}",
            input.display()
          )));
        }
      }
      self
        .run_module_on_session_vm(&session, &guard, &bundle, &args, options, context)
        .await
    } else {
      self
        .run_on_session_vm(&session, &guard, &source, &args, options, context)
        .await
    };

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
