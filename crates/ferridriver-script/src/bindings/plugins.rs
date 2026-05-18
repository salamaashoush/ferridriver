//! Plugin bindings -- expose loaded plugins as `plugins.<name>(args)` and
//! the allow-listed `commands.run(name, vars)` escape hatch.
//!
//! Plugins are passed to `install_plugins` as `PluginBinding` snapshots
//! (precompiled bytecode + per-tool allow-lists). Each file's bytecode is
//! the rolldown-bundled module produced once at startup by
//! [`crate::bundle::compile_and_extract_plugins`]; loading + evaluating it
//! publishes the file's tool array (with live handler closures) at
//! `globalThis.__ferri_plugin_files[i]`. For each tool a thin wrapper is
//! synthesised that looks its handler up by `(fileIndex, toolIndex)` and
//! invokes it with `{ args, page, context, request, commands }`.
//!
//! The `commands.run` callback dispatches into the single
//! `__ferri_plugin_commands` runner, which executes the matching template
//! via `sh -c` and parses the output as JSON when possible, plain text
//! otherwise. The allow-list lives entirely inside the wrapper closure
//! so a handler cannot escape into another plugin's commands.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use rquickjs::{Ctx, JsLifetime, Module, Object, Value, class::Class, class::Trace, function::Opt};

use crate::bindings::convert::{serde_from_js, serde_to_js};

/// Snapshot of one plugin source file handed to the script engine at
/// `install_plugins` time. A file may contribute one or more tools.
/// Lives in `ferridriver-script` so the crate stays self-contained --
/// the MCP crate maps its `LoadedPlugin` files into this shape before
/// invoking `engine.run`.
#[derive(Debug, Clone)]
pub struct PluginBinding {
  /// Pre-compiled `QuickJS` bytecode of the rolldown-bundled plugin
  /// module, produced once at startup by
  /// [`crate::bundle::compile_and_extract_plugins`]. A session VM
  /// `Module::load`s this — no per-session parse, no source retained.
  pub bytecode: Arc<[u8]>,
  /// Tools the file declares, in source order. Each maps onto a
  /// separate `plugins.<name>` binding.
  pub tools: Vec<PluginToolBinding>,
}

/// One tool declared inside a plugin file. Capabilities are per-tool so
/// a handler can only use what its own manifest authorises, even when a
/// sibling tool in the same file grants more.
#[derive(Debug, Clone, Default)]
pub struct PluginToolBinding {
  pub name: String,
  /// Allowed command templates, keyed by the name the handler uses with
  /// `commands.run(name, vars)`. Each value is a shell command template
  /// with `${var}` placeholders substituted from the call-time `vars`.
  pub allowed_commands: HashMap<String, String>,
  /// Host patterns the handler's `request` client may target (exact host
  /// or `*.suffix`). Empty = unrestricted; non-empty flips `request` to
  /// default-deny for this tool.
  pub allowed_net: Vec<String>,
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
/// One file's bytecode is loaded + evaluated exactly once per session
/// (under `globalThis.__ferri_plugin_files[i]`), then each tool gets its
/// own wrapper that looks the handler up by index. Sibling tools in the
/// same file share module-scoped helpers/constants for free.
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
    // 1. Load + evaluate the file's precompiled bytecode module. Its
    //    appended epilogue publishes the normalised tool array (with
    //    live handler closures) at `__ferri_plugin_files[file_idx]`.
    //
    // SAFETY: `file.bytecode` was produced by `Module::write` in THIS
    // process and this exact rquickjs/QuickJS build with native
    // endianness (see `compile_and_extract_plugins`) and is never
    // persisted or sent anywhere — the precondition `Module::load`
    // documents. A foreign or corrupt blob cannot reach this call.
    #[allow(unsafe_code)]
    let module = unsafe { Module::load(ctx.clone(), &file.bytecode) }?;
    // Bundled module body + epilogue are fully synchronous (no top-level
    // await); the slot assignment has applied by the time `eval`
    // returns, so the resolved promise carries nothing we need.
    let (_evaluated, _promise) = module.eval()?;

    // 2. For each tool, install a wrapper that locates its handler by
    //    (fileIndex, toolIndex) and binds the per-tool allow-list.
    for (tool_idx, tool) in file.tools.iter().enumerate() {
      let allowed_json = serde_json::to_string(&tool.allowed_commands).unwrap_or_else(|_| "{}".into());
      let net_json = serde_json::to_string(&tool.allowed_net).unwrap_or_else(|_| "[]".into());
      let name_literal = serde_json::to_string(&tool.name).unwrap_or_else(|_| "\"\"".into());

      // `request` is wrapped in a host-checking Proxy ONLY when the tool
      // declares `allow.net`; an empty list passes the binding through
      // untouched so the common (no-net) case has zero overhead and
      // unchanged semantics. Host match: exact, or `*.suffix` (which
      // also matches the bare apex).
      let wrapper = format!(
        "(() => {{\n\
           const ALLOWED = {allowed_json};\n\
           const NET = {net_json};\n\
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
           const __hostOk = (h) => NET.some((p) =>\n\
             p === h || (p.startsWith('*.') && (h === p.slice(2) || h.endsWith(p.slice(1)))));\n\
           const __NET_METHODS = ['get', 'post', 'put', 'delete', 'patch', 'head', 'fetch'];\n\
           const __guardedRequest = (req) => new Proxy(req, {{\n\
             get(t, prop) {{\n\
               const v = t[prop];\n\
               if (typeof v === 'function' && __NET_METHODS.includes(prop)) {{\n\
                 return (url, ...rest) => {{\n\
                   let host;\n\
                   try {{ host = new URL(url).hostname; }} catch (_) {{\n\
                     throw new Error('plugin ' + PLUGIN_NAME + ': request to invalid/relative URL \"' + url + '\" is not permitted by allow.net');\n\
                   }}\n\
                   if (!__hostOk(host)) {{\n\
                     throw new Error('plugin ' + PLUGIN_NAME + ': request host \"' + host + '\" is not in allow.net ' + JSON.stringify(NET));\n\
                   }}\n\
                   return t[prop](url, ...rest);\n\
                 }};\n\
               }}\n\
               return v;\n\
             }},\n\
           }});\n\
           return async (callArgs) => {{\n\
             const tool = globalThis.__ferri_plugin_files[FILE_IDX][TOOL_IDX];\n\
             const handler = tool && tool.handler;\n\
             if (typeof handler !== 'function') {{\n\
               throw new Error('plugin ' + PLUGIN_NAME + ' missing handler');\n\
             }}\n\
             const __req = globalThis.request;\n\
             return handler({{\n\
               args: callArgs,\n\
               page: globalThis.page,\n\
               context: globalThis.context,\n\
               request: (NET.length === 0 || !__req) ? __req : __guardedRequest(__req),\n\
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
