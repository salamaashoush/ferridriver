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

use rquickjs::{Ctx, JsLifetime, Object, Value, class::Class, class::Trace, function::Opt};
use serde::{Deserialize, Serialize};

use crate::bindings::convert::{serde_from_js, serde_to_js};

/// Snapshot of one loaded plugin handed to the script engine at
/// `install_plugins` time. Lives in `ferridriver-script` so the crate
/// stays self-contained -- the MCP crate maps its `LoadedPlugin` into
/// this shape before invoking `engine.run`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginBinding {
  pub name: String,
  pub source: String,
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
/// runner. `plugins` is an object keyed by manifest name; each value is
/// an async function `(args) => result`.
pub fn install_plugins(ctx: &Ctx<'_>, plugins: &[PluginBinding]) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  // Always install the runner -- even with zero plugins -- so handlers
  // copied between contexts at runtime never see a missing global.
  Class::<PluginCommandsJs>::define(&globals)?;
  let runner = Class::instance(ctx.clone(), PluginCommandsJs {})?;
  globals.set("__ferri_plugin_commands", runner)?;

  let plugins_obj = Object::new(ctx.clone())?;
  globals.set("plugins", plugins_obj.clone())?;

  for binding in plugins {
    let allowed_json = serde_json::to_string(&binding.allowed_commands).unwrap_or_else(|_| "{}".into());
    let source_literal = serde_json::to_string(&binding.source).unwrap_or_else(|_| "\"\"".into());
    let name_literal = serde_json::to_string(&binding.name).unwrap_or_else(|_| "\"\"".into());

    let wrapper = format!(
      "(() => {{\n\
         const ALLOWED = {allowed_json};\n\
         const PLUGIN_NAME = {name_literal};\n\
         const commands = {{\n\
           run: async (cmdName, vars) => {{\n\
             const tpl = ALLOWED[cmdName];\n\
             if (!tpl) throw new Error('commands.run: \"' + cmdName + '\" is not in the allow-list for plugin ' + PLUGIN_NAME);\n\
             return await globalThis.__ferri_plugin_commands.exec(tpl, vars || {{}});\n\
           }},\n\
         }};\n\
         const __pluginExports = {{}};\n\
         globalThis.exports = __pluginExports;\n\
         (0, eval)({source_literal});\n\
         const exp = globalThis.exports || __pluginExports;\n\
         delete globalThis.exports;\n\
         const handler = exp && exp.handler;\n\
         if (typeof handler !== 'function') {{\n\
           throw new Error('plugin ' + PLUGIN_NAME + ' missing handler');\n\
         }}\n\
         return async (callArgs) => handler({{\n\
           args: callArgs,\n\
           page: globalThis.page,\n\
           context: globalThis.context,\n\
           request: globalThis.request,\n\
           commands,\n\
         }});\n\
       }})()\n"
    );

    let fn_value: Value<'_> = ctx.eval(wrapper.as_bytes())?;
    plugins_obj.set(binding.name.as_str(), fn_value)?;
  }

  Ok(())
}
