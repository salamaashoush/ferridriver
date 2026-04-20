//! `ScriptEngine`: per-call `QuickJS` runtime + sandboxed context.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Function, Object, Value, async_with};

use crate::console::{ConsoleCapture, strip_ansi};
use crate::error::{ScriptError, ScriptErrorKind};
use crate::fs::PathSandbox;
use crate::result::{ConsoleLevel, ScriptResult};
use crate::vars::VarsStore;

/// Default console-capture limits.
pub const DEFAULT_MAX_CONSOLE_ENTRIES: usize = 1_000;
pub const DEFAULT_MAX_CONSOLE_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_CONSOLE_ENTRY_BYTES: usize = 8_192;

/// Default per-script wall-clock timeout (5 minutes).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Default per-script memory quota (256 MiB).
pub const DEFAULT_MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// Default per-script JS stack size (1 MiB).
pub const DEFAULT_STACK_SIZE: usize = 1024 * 1024;

/// Configuration for the script engine.
#[derive(Debug, Clone)]
pub struct ScriptEngineConfig {
  pub default_timeout: Duration,
  pub default_memory_limit: usize,
  pub default_stack_size: usize,
  pub max_console_entries: usize,
  pub max_console_bytes: usize,
  pub max_console_entry_bytes: usize,
}

impl Default for ScriptEngineConfig {
  fn default() -> Self {
    Self {
      default_timeout: DEFAULT_TIMEOUT,
      default_memory_limit: DEFAULT_MEMORY_LIMIT,
      default_stack_size: DEFAULT_STACK_SIZE,
      max_console_entries: DEFAULT_MAX_CONSOLE_ENTRIES,
      max_console_bytes: DEFAULT_MAX_CONSOLE_BYTES,
      max_console_entry_bytes: DEFAULT_MAX_CONSOLE_ENTRY_BYTES,
    }
  }
}

/// Per-call overrides for a single `run` invocation.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
  pub timeout: Option<Duration>,
  pub memory_limit: Option<usize>,
  pub stack_size: Option<usize>,
}

/// Per-call execution context holding session-level state the script reaches
/// via globals (`vars`, `fs`, `artifacts`, and the optional browser bindings
/// `page` / `context` / `request`). A `None` entry skips installation of
/// the matching global so pure-compute scripts don't need the extra
/// infrastructure.
#[derive(Clone)]
pub struct RunContext {
  pub vars: Arc<dyn VarsStore>,
  pub sandbox: Arc<PathSandbox>,
  /// Optional dedicated output directory, exposed to scripts as `artifacts`.
  /// Typically `.ferridriver/artifacts/` alongside `script_root`.
  pub artifacts: Option<Arc<PathSandbox>>,
  pub page: Option<Arc<ferridriver::Page>>,
  pub browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  pub request: Option<Arc<ferridriver::api_request::APIRequestContext>>,
}

/// Sandboxed `QuickJS` scripting engine.
pub struct ScriptEngine {
  config: ScriptEngineConfig,
}

impl ScriptEngine {
  #[must_use]
  pub fn new(config: ScriptEngineConfig) -> Self {
    Self { config }
  }

  #[must_use]
  pub fn config(&self) -> &ScriptEngineConfig {
    &self.config
  }

  /// Run a script with bound args.
  ///
  /// `source` is the JS text. `args` is an array of values made available
  /// inside the script as the `args` global (positional). Args are never
  /// interpolated into `source` — preventing prompt injection.
  pub async fn run(
    &self,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: RunContext,
  ) -> ScriptResult {
    let started = Instant::now();
    let timeout = options.timeout.unwrap_or(self.config.default_timeout);
    let memory_limit = options.memory_limit.unwrap_or(self.config.default_memory_limit);
    let stack_size = options.stack_size.unwrap_or(self.config.default_stack_size);

    let console = Arc::new(ConsoleCapture::new(
      self.config.max_console_entries,
      self.config.max_console_bytes,
      self.config.max_console_entry_bytes,
    ));

    let args_json = match serde_json::to_string(args) {
      Ok(s) => s,
      Err(e) => {
        return ScriptResult::err(
          ScriptError::internal(format!("failed to serialize args: {e}")),
          elapsed_ms(started),
          console.drain(),
        );
      },
    };

    let runtime = match AsyncRuntime::new() {
      Ok(r) => r,
      Err(e) => {
        return ScriptResult::err(
          ScriptError::internal(format!("rquickjs runtime init: {e}")),
          elapsed_ms(started),
          console.drain(),
        );
      },
    };

    runtime.set_memory_limit(memory_limit).await;
    runtime.set_max_stack_size(stack_size).await;

    // Module loader rooted at the sandbox — lets scripts `import './x.js'`.
    // Resolver and loader both check containment; rquickjs's built-in
    // ScriptLoader is replaced with our sandboxed pair so a rogue import
    // can't escape `script_root`.
    runtime
      .set_loader(
        crate::modules::SandboxResolver::new(context.sandbox.clone()),
        crate::modules::SandboxLoader::new(context.sandbox.clone()),
      )
      .await;

    // Timeout enforcement via interrupt handler. The handler fires regularly
    // during script execution; once the deadline passes we signal `true` to
    // halt the interpreter. `timed_out` is used to distinguish timeout from
    // other interruptions when we build the error result.
    let deadline = started + timeout;
    let timed_out = Arc::new(AtomicBool::new(false));
    {
      let timed_out = timed_out.clone();
      runtime
        .set_interrupt_handler(Some(Box::new(move || {
          if Instant::now() >= deadline {
            timed_out.store(true, Ordering::Relaxed);
            true
          } else {
            false
          }
        })))
        .await;
    }

    let ctx = match AsyncContext::full(&runtime).await {
      Ok(c) => c,
      Err(e) => {
        return ScriptResult::err(
          ScriptError::internal(format!("rquickjs context init: {e}")),
          elapsed_ms(started),
          console.drain(),
        );
      },
    };

    let install = GlobalsInstall {
      console: console.clone(),
      vars: context.vars.clone(),
      sandbox: context.sandbox.clone(),
      artifacts: context.artifacts.clone(),
      page: context.page.clone(),
      browser_context: context.browser_context.clone(),
      request: context.request.clone(),
      async_ctx: ctx.clone(),
    };
    let source_owned = source.to_string();

    let eval_result: Result<serde_json::Value, ScriptError> = async_with!(ctx => |ctx| {
      if let Err(e) = install_globals(&ctx, &args_json, install) {
        return Err(ScriptError::internal(format!("failed to install globals: {e}")));
      }

      let wrapped = wrap_source(&source_owned);

      let promise: rquickjs::Promise<'_> = match ctx.eval(wrapped.as_bytes()) {
        Ok(v) => v,
        Err(e) => return Err(caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), &source_owned)),
      };

      let result: Value<'_> = match promise.into_future::<Value<'_>>().await {
        Ok(v) => v,
        Err(e) => return Err(caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), &source_owned)),
      };

      Ok(value_to_json(&ctx, result).unwrap_or(serde_json::Value::Null))
    })
    .await;

    let duration = elapsed_ms(started);
    let drained = console.drain();

    match eval_result {
      Ok(value) => ScriptResult::ok(value, duration, drained),
      Err(mut err) => {
        if timed_out.load(Ordering::Relaxed) {
          err = ScriptError::timeout(duration, timeout.as_millis() as u64);
        }
        ScriptResult::err(err, duration, drained)
      },
    }
  }
}

/// Wrap user source in an async IIFE so `await` works at the top level and
/// the expression evaluates to a `Promise<value>` the engine can await.
fn wrap_source(source: &str) -> String {
  format!("(async () => {{\n{source}\n}})()")
}

/// Everything `install_globals` needs beyond `ctx` + args JSON. Bundled into
/// a struct so the helper stays under the clippy arity limit as the binding
/// surface grows.
struct GlobalsInstall {
  console: Arc<ConsoleCapture>,
  vars: Arc<dyn VarsStore>,
  sandbox: Arc<PathSandbox>,
  artifacts: Option<Arc<PathSandbox>>,
  page: Option<Arc<ferridriver::Page>>,
  browser_context: Option<Arc<ferridriver::context::ContextRef>>,
  request: Option<Arc<ferridriver::api_request::APIRequestContext>>,
  /// `AsyncContext` driving the script — passed to `install_page` so
  /// `page.route` callbacks can dispatch back into JS from a separate
  /// tokio task. Always present (cloned from the engine's context).
  async_ctx: AsyncContext,
}

/// Install the sandbox globals: `args`, `console`, `vars`, `fs`, and any of
/// `artifacts` / `page` / `context` / `request` that the run context carries.
fn install_globals(ctx: &Ctx<'_>, args_json: &str, inst: GlobalsInstall) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  // args: deserialise via JSON.parse so array/object args round-trip cleanly
  // without per-type conversion code.
  let args_src = format!("JSON.parse({})", json_literal(args_json));
  let args_value: Value<'_> = ctx.eval(args_src.as_bytes())?;
  globals.set("args", args_value)?;

  install_console(ctx, inst.console)?;
  install_vars(ctx, inst.vars)?;
  install_fs(ctx, inst.sandbox)?;

  if let Some(artifacts) = inst.artifacts {
    crate::bindings::install_artifacts(ctx, artifacts)?;
  }
  if let Some(page) = inst.page {
    crate::bindings::install_page(ctx, page, inst.async_ctx.clone())?;
  }
  if let Some(bcx) = inst.browser_context {
    crate::bindings::install_browser_context(ctx, bcx)?;
  }
  if let Some(req) = inst.request {
    crate::bindings::install_request(ctx, req)?;
  }

  Ok(())
}

/// Shell-safe quote a JSON string as a JS string literal.
fn json_literal(json: &str) -> String {
  let escaped = json.replace('\\', r"\\").replace('`', r"\`").replace("${", r"\${");
  format!("`{escaped}`")
}

fn install_console(ctx: &Ctx<'_>, capture: Arc<ConsoleCapture>) -> rquickjs::Result<()> {
  let native = Object::new(ctx.clone())?;

  for (name, level) in [
    ("log", ConsoleLevel::Log),
    ("info", ConsoleLevel::Info),
    ("warn", ConsoleLevel::Warn),
    ("error", ConsoleLevel::Error),
    ("debug", ConsoleLevel::Debug),
  ] {
    let cap = capture.clone();
    native.set(
      name,
      Func::from(move |msg: String| {
        cap.push(level, strip_ansi(&msg));
      }),
    )?;
  }

  ctx.globals().set("__ferridriver_console_native", native)?;

  // JS-side formatter that turns variadic args into a single display string,
  // matching the usual console semantics (JSON.stringify for objects).
  ctx.eval::<(), _>(
    r"
    globalThis.console = (() => {
      const native = globalThis.__ferridriver_console_native;
      const fmt = (a) => {
        if (a === null) return 'null';
        if (a === undefined) return 'undefined';
        if (typeof a === 'string') return a;
        if (typeof a === 'number' || typeof a === 'boolean' || typeof a === 'bigint') return String(a);
        if (typeof a === 'function') return a.toString();
        if (typeof a === 'symbol') return a.toString();
        try { return JSON.stringify(a); } catch (_) { return String(a); }
      };
      const write = (level, parts) => native[level](parts.map(fmt).join(' '));
      return {
        log:   (...a) => write('log', a),
        info:  (...a) => write('info', a),
        warn:  (...a) => write('warn', a),
        error: (...a) => write('error', a),
        debug: (...a) => write('debug', a),
      };
    })();
    delete globalThis.__ferridriver_console_native;
  "
    .as_bytes(),
  )?;

  Ok(())
}

fn install_vars(ctx: &Ctx<'_>, vars: Arc<dyn VarsStore>) -> rquickjs::Result<()> {
  let obj = Object::new(ctx.clone())?;

  {
    let v = vars.clone();
    obj.set("get", Func::from(move |name: String| v.get(&name)))?;
  }
  {
    let v = vars.clone();
    obj.set(
      "set",
      Func::from(move |name: String, value: String| {
        v.set(&name, value);
      }),
    )?;
  }
  {
    let v = vars.clone();
    obj.set("has", Func::from(move |name: String| v.has(&name)))?;
  }
  {
    let v = vars.clone();
    obj.set(
      "delete",
      Func::from(move |name: String| {
        v.delete(&name);
      }),
    )?;
  }
  {
    let v = vars.clone();
    obj.set("keys", Func::from(move || v.keys()))?;
  }

  ctx.globals().set("vars", obj)?;
  Ok(())
}

fn install_fs(ctx: &Ctx<'_>, sandbox: Arc<PathSandbox>) -> rquickjs::Result<()> {
  let obj = Object::new(ctx.clone())?;

  {
    let sb = sandbox.clone();
    obj.set(
      "readFile",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readFile", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "readFileBytes",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::read(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readFileBytes", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "writeFile",
      Func::from(Async(move |path: String, contents: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_write(&path).map_err(|e| to_rq_error(&e))?;
          tokio::fs::write(&resolved, contents)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "writeFile", e.to_string()))
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "readdir",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          let resolved = sb.resolve_read(&path).map_err(|e| to_rq_error(&e))?;
          let mut entries = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readdir", e.to_string()))?;
          let mut names: Vec<String> = Vec::new();
          while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| rquickjs::Error::new_from_js_message("fs", "readdir", e.to_string()))?
          {
            names.push(entry.file_name().to_string_lossy().into_owned());
          }
          Ok::<_, rquickjs::Error>(names)
        }
      })),
    )?;
  }
  {
    let sb = sandbox.clone();
    obj.set(
      "exists",
      Func::from(Async(move |path: String| {
        let sb = sb.clone();
        async move {
          // Syntactic checks still apply; absence or sandbox-escape returns false.
          match sb.resolve_read(&path) {
            Ok(resolved) => Ok::<bool, rquickjs::Error>(tokio::fs::try_exists(&resolved).await.unwrap_or(false)),
            Err(_) => Ok(false),
          }
        }
      })),
    )?;
  }

  // Expose the sandbox root so scripts can build relative paths confidently.
  obj.set("root", sandbox.root().to_string_lossy().into_owned())?;

  ctx.globals().set("fs", obj)?;
  Ok(())
}

fn to_rq_error(err: &ScriptError) -> rquickjs::Error {
  // The `from`/`to` static labels are used only in rquickjs's Display impl.
  // We route sandbox-rejection errors through the FromJs variant so the
  // message propagates to JS as a thrown exception with our reason string.
  rquickjs::Error::new_from_js_message("fs", "sandbox", err.message.clone())
}

fn value_to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Option<serde_json::Value> {
  // Round-trip through JSON.stringify; handles arrays/objects/primitives
  // uniformly without per-type conversion.
  let json_global: Object<'js> = ctx.globals().get("JSON").ok()?;
  let stringify: Function<'js> = json_global.get("stringify").ok()?;
  let serialized: Option<String> = stringify.call((value,)).ok();
  serialized.and_then(|s| serde_json::from_str(&s).ok())
}

fn caught_to_script_error(caught: rquickjs::CaughtError<'_>, source: &str) -> ScriptError {
  let (message, stack, line, column) = match caught {
    rquickjs::CaughtError::Exception(ex) => {
      let message = ex.message().unwrap_or_else(|| "exception".to_string());
      let stack = ex.stack();
      // Playwright-style: lineNumber/columnNumber are present on most QuickJS
      // exceptions; read them directly off the exception object.
      let obj = ex.as_object();
      let line = obj.get::<_, u32>("lineNumber").ok();
      let column = obj.get::<_, u32>("columnNumber").ok();
      (message, stack, line, column)
    },
    rquickjs::CaughtError::Value(v) => (format!("{v:?}"), None, None, None),
    rquickjs::CaughtError::Error(e) => (format!("{e}"), None, None, None),
  };

  ScriptError {
    kind: ScriptErrorKind::Runtime,
    message,
    stack,
    line,
    column,
    source_snippet: line.and_then(|l| snippet_around_line(source, l, 2)),
  }
}

/// Build a 1-indexed source snippet with `context_lines` around the target
/// line, used in error reporting so the LLM can see where the script failed.
fn snippet_around_line(source: &str, line_1based: u32, context_lines: u32) -> Option<String> {
  use std::fmt::Write as _;
  let lines: Vec<&str> = source.lines().collect();
  if lines.is_empty() {
    return None;
  }
  let target = line_1based.saturating_sub(1) as usize;
  let start = target.saturating_sub(context_lines as usize);
  let end = (target + context_lines as usize + 1).min(lines.len());
  let mut out = String::new();
  for (i, text) in lines[start..end].iter().enumerate() {
    let ln = start + i + 1;
    let marker = if ln == line_1based as usize { ">>>" } else { "   " };
    let _ = writeln!(out, "{marker} {ln:>4}: {text}");
  }
  Some(out)
}

fn elapsed_ms(started: Instant) -> u64 {
  started.elapsed().as_millis() as u64
}
