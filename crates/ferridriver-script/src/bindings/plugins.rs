//! Plugin bindings -- expose loaded plugins as `plugins.<name>(args)` and
//! the allow-listed `commands.run(name, vars)` escape hatch.
//!
//! Plugins are passed to `install_plugins` as `PluginBinding` snapshots
//! (name, source text, allowed command map). For each plugin a JS wrapper
//! is synthesised: it evaluates the plugin source so its `globalThis.exports`
//! is set, extracts the handler, and returns a closure that invokes the
//! handler with `{ args, page, context, request, commands }`.
//!
//! The `commands.run` callback dispatches into the single
//! `__ferri_plugin_commands` runner, which executes the matching template
//! via `sh -c` and parses the output as JSON when possible, plain text
//! otherwise. The allow-list lives entirely inside the wrapper closure
//! so a handler cannot escape into another plugin's commands.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use rquickjs::{
  AsyncContext, AsyncRuntime, Ctx, JsLifetime, Module, Object, Value, WriteOptions, WriteOptionsEndianness,
  async_with, class::Class, class::Trace, function::Opt,
};

use crate::bindings::convert::{serde_from_js, serde_to_js};
use crate::error::ScriptError;

/// Snapshot of one plugin source file handed to the script engine at
/// `install_plugins` time. A file may contribute one or more tools.
/// Lives in `ferridriver-script` so the crate stays self-contained --
/// the MCP crate maps its `LoadedPlugin` files into this shape before
/// invoking `engine.run`.
#[derive(Debug, Clone)]
pub struct PluginBinding {
  /// Source text of the plugin file. Shared (`Arc`) so handing the
  /// binding to a session VM is a refcount bump, not a full copy of
  /// potentially-large plugin source. Used by the source-eval fallback
  /// when `bytecode` is `None`.
  pub source: Arc<str>,
  /// Pre-compiled `QuickJS` bytecode of the per-file wrapper module
  /// (produced once at plugin load by [`compile_plugin_bytecode`]).
  /// When present, a session VM loads this instead of parsing
  /// `source` — no per-session parse. `None` falls back to source eval.
  pub bytecode: Option<Arc<[u8]>>,
  /// Tools the file declares, in source order. Each maps onto a
  /// separate `plugins.<name>` binding.
  pub tools: Vec<PluginToolBinding>,
}

/// One tool declared inside a plugin file. The allow-list is per-tool
/// so a handler can only invoke commands the manifest explicitly
/// authorises, even when sibling tools in the same file grant more.
#[derive(Debug, Clone, Default)]
pub struct PluginToolBinding {
  pub name: String,
  /// Allowed command templates, keyed by the name the handler uses with
  /// `commands.run(name, vars)`. Each value is a shell command template
  /// with `${var}` placeholders substituted from the call-time `vars`.
  pub allowed_commands: HashMap<String, String>,
}

/// Singleton helper instantiated once per script run and exposed as the
/// hidden global `__ferri_plugin_commands`. JS wrappers call its `exec`
/// method via `commands.run` after consulting their private allow-list.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "PluginCommands")]
pub struct PluginCommandsJs {}

#[rquickjs::methods]
impl PluginCommandsJs {
  /// Execute a shell-command template after `${var}` substitution.
  ///
  /// `template` is the literal command string (already vetted by the
  /// per-plugin allow-list in the calling JS wrapper). `vars` is a plain
  /// JS object; each value is JSON-coerced and shell-single-quoted before
  /// interpolation. Output parsing mirrors the existing
  /// `instance_args_command` helper: JSON object/array preferred, raw
  /// trimmed string otherwise.
  #[qjs(rename = "exec")]
  pub async fn exec<'js>(
    &self,
    ctx: Ctx<'js>,
    template: String,
    vars: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let vars_map: HashMap<String, serde_json::Value> = match vars.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
      _ => HashMap::new(),
    };

    let mut cmd = template;
    for (key, value) in &vars_map {
      let placeholder = format!("${{{key}}}");
      let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
      };
      cmd = cmd.replace(&placeholder, &shell_single_quote(&raw));
    }

    let output = tokio::task::spawn_blocking(move || Command::new("sh").args(["-c", &cmd]).output())
      .await
      .map_err(|e| rquickjs::Error::new_from_js_message("commands.exec", "Error", e.to_string()))?
      .map_err(|e| rquickjs::Error::new_from_js_message("commands.exec", "Error", e.to_string()))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr).to_string();
      return Err(rquickjs::Error::new_from_js_message(
        "commands.exec",
        "Error",
        format!("command failed (exit {}): {stderr}", output.status),
      ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parsed = parse_command_output(&stdout);
    serde_to_js(&ctx, &parsed)
  }
}

fn shell_single_quote(s: &str) -> String {
  format!("'{}'", s.replace('\'', r"'\''"))
}

fn parse_command_output(s: &str) -> serde_json::Value {
  if s.is_empty() {
    return serde_json::Value::Null;
  }
  if s.starts_with('{') || s.starts_with('[') {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
      return v;
    }
  }
  serde_json::Value::String(s.to_string())
}

/// Install the `plugins` global and the hidden `__ferri_plugin_commands`
/// runner. `plugins` is an object keyed by tool name; each value is an
/// async function `(args) => result`.
///
/// One source file may declare several tools. The source is eval'd
/// exactly once per file (under `globalThis.__ferri_plugin_files[i]`),
/// then each tool gets its own wrapper that looks the handler up by
/// index. Sibling tools in the same file share globals (helpers,
/// constants) without re-evaluation overhead.
pub fn install_plugins(ctx: &Ctx<'_>, files: &[PluginBinding]) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  // Always install the runner -- even with zero plugins -- so handlers
  // copied between contexts at runtime never see a missing global.
  Class::<PluginCommandsJs>::define(&globals)?;
  let runner = Class::instance(ctx.clone(), PluginCommandsJs {})?;
  globals.set("__ferri_plugin_commands", runner)?;

  let plugins_obj = Object::new(ctx.clone())?;
  globals.set("plugins", plugins_obj.clone())?;

  // Storage for per-file tool arrays (the eval'd manifests, with their
  // live handler closures). Indexed by file position; the per-tool
  // wrapper closes over (fileIndex, toolIndex) to look up its handler.
  ctx.eval::<(), _>(b"globalThis.__ferri_plugin_files = [];".as_slice())?;

  for (file_idx, file) in files.iter().enumerate() {
    // 1. Make `globalThis.__ferri_plugin_files[file_idx]` the file's
    //    normalised tool array. Prefer the pre-compiled bytecode module
    //    (no per-session parse); fall back to evaluating the source
    //    wrapper when bytecode is absent or failed to compile.
    if let Some(bytecode) = file.bytecode.as_deref() {
      // SAFETY: `bytecode` was produced by `Module::write` on a
      // compile-only module in `compile_plugin_bytecode`, within THIS
      // process and this exact rquickjs/QuickJS build, using Native
      // endianness, and is never persisted or sent anywhere. So
      // `JS_ReadObject` receives well-formed bytecode for this very
      // interpreter — the precondition `Module::load` documents. A
      // foreign or corrupt blob cannot reach this call.
      #[allow(unsafe_code)]
      let module = unsafe { Module::load(ctx.clone(), bytecode) }?;
      // The wrapper module body is fully synchronous (no top-level
      // await); its only effect — assigning
      // `globalThis.__ferri_plugin_files[file_idx]` — has already
      // applied by the time `eval` returns, so the resolved promise
      // carries nothing we need.
      let (_evaluated, _promise) = module.eval()?;
    } else {
      let source_literal = serde_json::to_string(&*file.source).unwrap_or_else(|_| "\"\"".into());
      ctx.eval::<(), _>(plugin_file_wrapper(file_idx, &source_literal).as_bytes())?;
    }

    // 2. For each tool, install a wrapper that locates its handler by
    //    (fileIndex, toolIndex) and binds the per-tool allow-list.
    for (tool_idx, tool) in file.tools.iter().enumerate() {
      let allowed_json = serde_json::to_string(&tool.allowed_commands).unwrap_or_else(|_| "{}".into());
      let name_literal = serde_json::to_string(&tool.name).unwrap_or_else(|_| "\"\"".into());

      let wrapper = format!(
        "(() => {{\n\
           const ALLOWED = {allowed_json};\n\
           const PLUGIN_NAME = {name_literal};\n\
           const FILE_IDX = {file_idx};\n\
           const TOOL_IDX = {tool_idx};\n\
           const commands = {{\n\
             run: async (cmdName, vars) => {{\n\
               const tpl = ALLOWED[cmdName];\n\
               if (!tpl) throw new Error('commands.run: \"' + cmdName + '\" is not in the allow-list for plugin ' + PLUGIN_NAME);\n\
               return await globalThis.__ferri_plugin_commands.exec(tpl, vars || {{}});\n\
             }},\n\
           }};\n\
           return async (callArgs) => {{\n\
             const tool = globalThis.__ferri_plugin_files[FILE_IDX][TOOL_IDX];\n\
             const handler = tool && tool.handler;\n\
             if (typeof handler !== 'function') {{\n\
               throw new Error('plugin ' + PLUGIN_NAME + ' missing handler');\n\
             }}\n\
             return handler({{\n\
               args: callArgs,\n\
               page: globalThis.page,\n\
               context: globalThis.context,\n\
               request: globalThis.request,\n\
               commands,\n\
             }});\n\
           }};\n\
         }})()\n"
      );

      let fn_value: Value<'_> = ctx.eval(wrapper.as_bytes())?;
      plugins_obj.set(tool.name.as_str(), fn_value)?;
    }
  }

  Ok(())
}

/// The per-file wrapper: evaluate the plugin source once (in sloppy
/// global scope via indirect `eval`, preserving classic-script
/// semantics), normalise `globalThis.exports` into a tool array, and
/// publish it at `globalThis.__ferri_plugin_files[file_idx]`.
///
/// Single source of truth for both the source-eval fallback and the
/// bytecode the loader pre-compiles — they MUST stay byte-identical so
/// a file's tool indices line up regardless of which path ran.
fn plugin_file_wrapper(file_idx: usize, source_literal: &str) -> String {
  format!(
    "(() => {{\n\
       const __exp_before = globalThis.exports;\n\
       globalThis.exports = undefined;\n\
       (0, eval)({source_literal});\n\
       const __exp = globalThis.exports;\n\
       globalThis.exports = __exp_before;\n\
       if (typeof __exp !== 'object' || __exp === null) {{\n\
         throw new Error('plugin file index {file_idx} did not set globalThis.exports');\n\
       }}\n\
       let __tools;\n\
       if (Array.isArray(__exp)) __tools = __exp;\n\
       else if (Array.isArray(__exp.tools)) __tools = __exp.tools;\n\
       else __tools = [__exp];\n\
       globalThis.__ferri_plugin_files[{file_idx}] = __tools;\n\
     }})()\n"
  )
}

/// Compile one plugin file's wrapper to `QuickJS` bytecode, once, at
/// plugin load time. The bytes are loaded (not parsed) into every
/// session VM via the `unsafe Module::load` path in [`install_plugins`].
///
/// `file_idx` must equal the file's position in the registry's file
/// list — it is baked into the wrapper so the bytecode publishes to the
/// correct `__ferri_plugin_files` slot. A throwaway runtime/context is
/// used purely to compile (`JS_EVAL_FLAG_COMPILE_ONLY`); nothing runs.
///
/// # Errors
///
/// Returns [`ScriptError`] if the throwaway VM cannot be created or the
/// wrapper fails to compile/serialise. Callers treat this as a soft
/// failure and fall back to source eval.
pub async fn compile_plugin_bytecode(file_idx: usize, source: &str, module_name: &str) -> Result<Vec<u8>, ScriptError> {
  let source_literal = serde_json::to_string(source).unwrap_or_else(|_| "\"\"".into());
  let wrapper = plugin_file_wrapper(file_idx, &source_literal);

  let runtime =
    AsyncRuntime::new().map_err(|e| ScriptError::internal(format!("bytecode runtime init: {e}")))?;
  let ctx = AsyncContext::full(&runtime)
    .await
    .map_err(|e| ScriptError::internal(format!("bytecode context init: {e}")))?;

  let name = module_name.to_string();
  async_with!(ctx => |ctx| {
    let module = Module::declare(ctx.clone(), name.into_bytes(), wrapper.into_bytes())
      .map_err(|e| ScriptError::internal(format!("plugin module compile: {e}")))?;
    module
      .write(WriteOptions {
        // Same process + interpreter that will `load` it; native byte
        // order avoids a pointless byte-swap.
        endianness: WriteOptionsEndianness::Native,
        ..Default::default()
      })
      .map_err(|e| ScriptError::internal(format!("plugin module write: {e}")))
  })
  .await
}
