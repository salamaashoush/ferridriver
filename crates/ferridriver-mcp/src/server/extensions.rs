//! Extension discovery, hot reload, promotion, and invocation for [`McpServer`].

use super::McpServer;
use rmcp::ErrorData;
use std::sync::Arc;

/// The loaded extension set: the registry AND the tool list built from
/// it, published together.
///
/// One `ArcSwap`, not two. Held separately, a `call_tool` landing between
/// the two stores saw the new registry with the previous listing (or the
/// reverse), so a reload could advertise a tool the registry no longer
/// had — which is exactly the window a reload exists to close.
#[derive(Default)]
pub(crate) struct LoadedExtensions {
  pub registry: crate::extension::ExtensionRegistry,
  /// The `tools/list` entries, kept beside the static tool router rather
  /// than inside it: `ToolRouter` routes are fixed once the server is
  /// serving, and an authoring loop needs the promoted set to change
  /// without a restart. Dispatch for these names goes straight to
  /// [`McpServer::invoke_extension_tool`], the same path the router's
  /// dynamic route used.
  pub promoted: Vec<rmcp::model::Tool>,
}

impl McpServer {
  /// Discover and load every configured extension file as MCP tools.
  /// `specs` come from the top-level `extensions` config: each is a
  /// source file, source directory, package directory, or ESM package
  /// specifier resolved from `node_modules`.
  ///
  /// Failed extensions are logged and skipped -- one broken file should
  /// not prevent the server from starting. Successfully loaded tools are
  /// stored in the extension registry and become available as `run_script`
  /// bindings (and, when `exposeAsMcpTool`, as MCP tools).
  pub async fn load_extensions(&mut self, specs: &[ferridriver_config::ExtensionSpec]) {
    // Record the specs so the BDD path can re-bundle them as step sources
    // even though run_script consumes them as already-loaded extensions.
    self.extension_specs = specs.to_vec();
    if specs.is_empty() {
      return;
    }
    self.load_extension_specs().await;
  }

  /// Re-run discovery + load for the specs recorded at startup, replace
  /// the registry and the promoted tool set, and discard every live
  /// session VM so open sessions pick the new bytecode up on their next
  /// call (their durable `vars` / processes / cookies survive).
  ///
  /// Editing an extension previously meant restarting the MCP client,
  /// which drops every browser session with it — the authoring loop cost
  /// far more than the edit.
  ///
  /// Returns `(tool_count, dropped_vm_count)`.
  pub async fn reload_extensions(&self) -> (usize, usize) {
    // Serialised: concurrent reloads would both bundle and both publish,
    // and whichever finished last would win regardless of which source
    // tree the caller meant.
    let _serial = self.reload_lock.lock().await;
    self.load_extension_specs().await;
    let dropped = self.sessions.drop_all_vms().await;
    let count = self.extensions().registry.tool_count();
    tracing::info!(tools = count, dropped_vms = dropped, "reloaded extensions");
    (count, dropped)
  }

  /// The currently-loaded extension set: registry and advertised tools,
  /// as one consistent snapshot.
  pub(crate) fn extensions(&self) -> Arc<LoadedExtensions> {
    self.extension_registry.load_full()
  }

  /// Replace the registry (and the promoted tool set) from
  /// `self.extension_specs`. Shared by startup and reload so the two can
  /// never resolve, gate or promote differently.
  async fn load_extension_specs(&self) {
    let specs = self.extension_specs.clone();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut policy_warnings: Vec<(String, String)> = Vec::new();

    // One resolve + gate for every host (`ferridriver_script::extension_load`):
    // a package states its preconditions in `package.json`
    // (`ferridriver.requires` / `ferridriver.settings`), and the gate is
    // what turns a missing binary, an unlisted `allowEnv` name, a host
    // outside the operator ceiling, an undeclared sidecar or a mistyped
    // settings key into one message naming the package and the config
    // key that fixes it — instead of a failure on the first call.
    let sidecar_names: Vec<String> = self
      .script_engine
      .config()
      .sidecars
      .iter()
      .map(|s| s.name.clone())
      .collect();
    let env = ferridriver_script::RequirementEnv::from_caps(&self.script_caps, &sidecar_names);
    let gated = ferridriver_script::gate(&specs, &env, ferridriver_script::ExtensionHost::Mcp);
    for (spec, e) in &gated.resolve_errors {
      tracing::warn!(extension = %spec, error = %e.message, "extension discovery failed; skipping path");
      failures.push((spec.clone(), e.message.clone()));
    }
    for issue in &gated.issues {
      if issue.blocking {
        tracing::error!(source = %issue.source, "extension package requirement unmet: {}", issue.message);
        failures.push((issue.source.clone(), issue.message.clone()));
      } else {
        tracing::warn!(source = %issue.source, "{}", issue.message);
        policy_warnings.push((issue.source.clone(), issue.message.clone()));
      }
    }
    let files = gated.files.clone();

    let (loaded, errors) = if files.is_empty() {
      (Vec::new(), Vec::new())
    } else {
      crate::extension::load_all(&files, &self.script_caps.extension_policy).await
    };
    for e in errors {
      tracing::warn!(error = %e, "extension load failed; skipping");
      failures.push((e.source_label(), e.to_string()));
    }
    for lp in &loaded {
      let tool_names: Vec<&str> = lp.tools.iter().map(|t| t.name.as_str()).collect();
      tracing::info!(path = %lp.path.display(), tools = ?tool_names, "loaded extension file");
      if lp.tools.is_empty() {
        // Legitimate for an entry that only contributes BDD steps or
        // script-host globals — and the only signal when a `defineTool`
        // call did not run (a guard that was never true, a top-level
        // throw before it).
        let message = "declares no tools; loaded for its BDD steps / script-host contributions. \
                       If it was meant to register a tool, its defineTool(...) call never ran."
          .to_string();
        tracing::warn!(path = %lp.path.display(), "{message}");
        policy_warnings.push((lp.path.display().to_string(), message));
      }
    }

    let mut warnings = self.policy_conflicts(&loaded);
    for (source, message) in &warnings {
      tracing::warn!(source = %source, "{message}");
    }
    warnings.append(&mut policy_warnings);
    let registry = crate::extension::ExtensionRegistry::with_warnings(loaded, failures, warnings);
    let promoted = self.promoted_tool_list(&registry);
    // Registry and listing published in ONE store, so no call can observe
    // a listing that disagrees with the registry behind it.
    self
      .extension_registry
      .store(Arc::new(LoadedExtensions { registry, promoted }));
  }

  /// Lint every loaded manifest against the operator extension policy
  /// (`[extensions.policy]`, threaded in via [`Self::with_script_caps`],
  /// so call that first). Enforcement happens at session registration
  /// (`defineTool` clamps `allow.net`; a command-ceiling violation fails
  /// the tool); this pass makes each conflict visible at startup and
  /// through `ferridriver_extensions` instead of only at first dispatch.
  fn policy_conflicts(&self, loaded: &[crate::extension::LoadedExtension]) -> Vec<(String, String)> {
    use ferridriver_config::ExtensionCommandsCeiling as Ceiling;
    let policy = &self.script_caps.extension_policy;
    let mut warnings = Vec::new();
    for lp in loaded {
      let source = lp.path.display().to_string();
      for t in &lp.tools {
        if let Some(ceiling) = policy.net.as_deref() {
          let dropped: Vec<&str> = t
            .allow
            .net
            .iter()
            .map(String::as_str)
            .filter(|d| !ferridriver_script::net_entry_subsumed(d, ceiling))
            .collect();
          if !dropped.is_empty() {
            warnings.push((
              source.clone(),
              format!(
                "tool `{}`: allow.net entries {dropped:?} are outside the operator ceiling \
                 ([extensions.policy] net) and will be dropped from the effective grant",
                t.name
              ),
            ));
          }
        }
        let shell_form: Vec<&str> = t
          .allow
          .commands
          .iter()
          .filter(|(_, spec)| matches!(spec.run, ferridriver_script::CommandRun::Shell(_)))
          .map(|(name, _)| name.as_str())
          .collect();
        match policy.commands {
          Ceiling::ArgvOnly if !shell_form.is_empty() => warnings.push((
            source.clone(),
            format!(
              "tool `{}`: commands {shell_form:?} are shell-string specs, but the operator policy \
               permits only argv-array specs (`commands = \"argvOnly\"`) — the tool will fail to register",
              t.name
            ),
          )),
          Ceiling::None if !t.allow.commands.is_empty() => warnings.push((
            source.clone(),
            format!(
              "tool `{}` declares allow.commands, but the operator policy forbids command \
               declarations (`commands = \"none\"`) — the tool will fail to register",
              t.name
            ),
          )),
          Ceiling::Any | Ceiling::ArgvOnly | Ceiling::None => {},
        }
      }
    }
    warnings
  }

  /// The `tools/list` entries for every extension manifest that declares
  /// `exposeAsMcpTool: true`. Name, description and schemas come from the
  /// manifest; dispatch goes to [`Self::invoke_extension_tool`], so the
  /// tool path and the `run_script` binding path share one handler.
  ///
  /// Built as a plain list (not `ToolRouter` routes) because the set has
  /// to be replaceable while the server is serving — see
  /// [`LoadedExtensions::promoted`].
  fn promoted_tool_list(&self, registry: &crate::extension::ExtensionRegistry) -> Vec<rmcp::model::Tool> {
    use rmcp::model::Tool;

    let promoted: Vec<_> = registry
      .promoted_tools()
      .map(|t| {
        let name = t.name.clone();
        // Some MCP clients enforce tool-name patterns (commonly
        // `[a-zA-Z0-9_-]`); a dotted namespace or exotic character may
        // be rejected client-side even though the server accepts it.
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
          tracing::warn!(
            name = %name,
            "promoted tool name contains characters outside [a-zA-Z0-9_-]; some MCP clients reject such names"
          );
        }
        let as_schema_obj = |schema: Option<serde_json::Value>, label: &str| {
          let schema = schema?;
          if let serde_json::Value::Object(m) = schema {
            Some(Arc::new(m))
          } else {
            // tools/list would advertise nothing while invocation
            // validates against the real (non-object) schema — surface
            // the divergence to the author instead of leaving it silent.
            tracing::warn!(
              name = %name,
              "{label} is not a JSON object; omitting it from tools/list (calls still validate against the declared one)"
            );
            None
          }
        };
        let declared = as_schema_obj(t.input_schema.clone(), "inputSchema")
          .unwrap_or_else(|| Arc::new(serde_json::Map::new()));
        let input_schema = advertise_session(declared);
        let output_schema = as_schema_obj(t.output_schema.clone(), "outputSchema");
        let mut tool = Tool::new(name.clone(), t.description.clone().unwrap_or_default(), input_schema);
        tool.title.clone_from(&t.title);
        tool.output_schema = output_schema;
        tool.annotations.clone_from(&t.annotations);
        tool
      })
      .collect();

    let mut out: Vec<Tool> = Vec::with_capacity(promoted.len());
    for tool in promoted {
      let name = tool.name.to_string();
      // `register_tool` already rejects duplicate names within a load
      // batch; this guards the remaining collision: an extension name
      // that shadows a built-in tool. Skip + warn rather than shadowing
      // a built-in, which would silently change what `navigate` means.
      if self.tool_router.has_route(&name) {
        tracing::warn!(name = %name, "extension tool name collides with a built-in tool; not promoting");
        continue;
      }
      if out.iter().any(|t| t.name == tool.name) {
        tracing::warn!(name = %name, "duplicate promoted extension tool name; keeping the first");
        continue;
      }
      tracing::info!(name = %name, "promoted extension to MCP tool");
      out.push(tool);
    }
    out
  }

  /// Whether a promoted extension tool is currently advertised under `name`,
  /// without cloning the tool's schema or metadata.
  pub(super) fn has_promoted_extension_tool(&self, name: &str) -> bool {
    self.extensions().promoted.iter().any(|t| t.name == name)
  }

  /// The promoted extension tool advertised under `name`, if any.
  pub(super) fn promoted_extension_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
    self.extensions().promoted.iter().find(|t| t.name == name).cloned()
  }

  /// Names of the currently-advertised extension tools, in list order.
  pub(crate) fn promoted_tool_names(&self) -> Vec<String> {
    self.extensions().promoted.iter().map(|t| t.name.to_string()).collect()
  }

  /// Invoke an extension by manifest name with the given argument object.
  /// Backs both the `exposeAsMcpTool` registration and any direct caller
  /// that wants to dispatch an extension without writing JS by hand.
  ///
  /// `args_obj` is wrapped into a single positional `args[0]` for the
  /// underlying script run. The extension's `session` argument (if present)
  /// is honoured for browser context selection.
  ///
  /// # Errors
  ///
  /// Returns an [`ErrorData`] if the extension name is unknown, scripting
  /// is disabled (no usable script root), the underlying browser
  /// session cannot be established, or the final result fails to
  /// serialise.
  pub async fn invoke_extension_tool(
    &self,
    tool_name: &str,
    args_obj: serde_json::Value,
  ) -> Result<rmcp::model::CallToolResult, ErrorData> {
    use rmcp::model::CallToolResult;

    let loaded = self.extensions();
    let registry = &loaded.registry;
    if registry.get_tool(tool_name).is_none() {
      return Err(Self::err(format!("unknown extension: {tool_name}")));
    }
    // Enforce the declared inputSchema before doing any work (browser
    // launch, session lock). A non-conforming call is the caller's bug,
    // surfaced as a tool error so the model can correct and retry. The
    // validator was compiled once at load ([`crate::extension::ExtensionRegistry::new`]);
    // an invalid schema is the extension author's bug and is surfaced
    // loudly rather than silently skipped.
    if let Some(compiled) = registry.validator(tool_name) {
      match compiled {
        Err(msg) => return Ok(CallToolResult::error(vec![self.text(msg.clone())])),
        Ok(validator) => {
          // `session` is the reserved routing key (browser-session
          // selection), not part of the tool's declared contract —
          // validate against the args without it so a strict schema
          // (`additionalProperties: false`) does not reject session
          // routing. The handler still receives the full object.
          let validate_target = match &args_obj {
            serde_json::Value::Object(m) if m.contains_key("session") => {
              let mut m = m.clone();
              m.remove("session");
              std::borrow::Cow::Owned(serde_json::Value::Object(m))
            },
            other => std::borrow::Cow::Borrowed(other),
          };
          if let Err(msg) = validate_tool_args(tool_name, validator, &validate_target) {
            return Ok(CallToolResult::error(vec![self.text(msg)]));
          }
        },
      }
    }

    let session = args_obj
      .get("session")
      .and_then(|v| v.as_str())
      .map_or_else(|| "default".into(), str::to_string);
    // Serialize per-session tool calls so concurrent run_script and extension
    // invocations on the same session don't race against each other's
    // browser state (cookies, navigation, page identity). Matches the
    // pattern other tool routers use.
    let guard = self.session_guard(&session).await;
    let context = self.mcp_run_context(&session).await?;

    let result = Box::pin(self.run_tool_on_session_vm(
      &session,
      &guard,
      tool_name,
      args_obj,
      ferridriver_script::RunOptions::default(),
      context,
    ))
    .await;

    self.extension_tool_result(tool_name, &result)
  }

  /// Shape a finished extension run into the MCP tool reply.
  ///
  /// A handler failure surfaces as an MCP error result (`is_error`) so
  /// the model can distinguish it from success, not a "success"
  /// carrying an error blob (this deliberately differs from
  /// `run_script`, whose contract is "always succeed, inspect
  /// `status`"). On success, a declared `outputSchema` is the symmetric
  /// half of the schema contract: the handler's return value must
  /// conform (a violation is the AUTHOR's bug, surfaced as a tool
  /// error) and ships as MCP `structuredContent` alongside the text
  /// payload.
  fn extension_tool_result(
    &self,
    tool_name: &str,
    result: &ferridriver_script::ScriptResult,
  ) -> Result<rmcp::model::CallToolResult, ErrorData> {
    use rmcp::model::CallToolResult;

    let json = serde_json::to_string_pretty(result).map_err(|e| Self::err(format!("serialize result: {e}")))?;
    let mut contents = vec![self.text(json)];
    let success = match &result.outcome {
      ferridriver_script::Outcome::Error { error } => {
        let summary = format!("[{:?}] {} ({}ms)", error.kind, error.message, result.duration_ms);
        contents.insert(0, self.text(summary));
        return Ok(CallToolResult::error(contents));
      },
      ferridriver_script::Outcome::Ok { success } => success,
    };
    let mut out = CallToolResult::success(contents);
    let loaded = self.extensions();
    let registry = &loaded.registry;
    if let Some(compiled) = registry.output_validator(tool_name) {
      match compiled {
        Err(msg) => return Ok(CallToolResult::error(vec![self.text(msg.clone())])),
        Ok(validator) => {
          if let Some(messages) = schema_violations(validator, &success.value) {
            return Ok(CallToolResult::error(vec![self.text(format!(
              "`{tool_name}` returned a value that does not match its declared outputSchema \
               (the extension author's bug):\n- {messages}"
            ))]));
          }
          out.structured_content = Some(success.value.clone());
        },
      }
    }
    Ok(out)
  }

  /// Snapshot the loaded extension registry into the script-engine binding
  /// shape. Shared by `run_script` and `invoke_extension_tool` so the mapping
  /// lives in exactly one place.
  pub(crate) fn extension_bindings(&self) -> Vec<ferridriver_script::ExtensionBinding> {
    let loaded = self.extensions();
    let registry = &loaded.registry;
    registry
      .files()
      .iter()
      .map(|f| ferridriver_script::ExtensionBinding {
        bytecode: Arc::clone(&f.bytecode),
        name: f.path.display().to_string(),
        source_map: f.source_map.clone(),
        provides: ferridriver_script::provided_modules::provider_module_name(&f.path),
      })
      .collect()
  }
}

/// Every violation of `validator` in `value`, joined for display —
/// `None` when the value conforms.
fn schema_violations(validator: &jsonschema::Validator, value: &serde_json::Value) -> Option<String> {
  let mut messages: Vec<String> = validator
    .iter_errors(value)
    .map(|e| {
      let path = e.instance_path().to_string();
      if path.is_empty() {
        e.to_string()
      } else {
        format!("{path}: {e}")
      }
    })
    .take(20)
    .collect();
  if messages.is_empty() {
    return None;
  }
  messages.sort();
  messages.dedup();
  Some(messages.join("\n- "))
}

fn validate_tool_args(
  extension: &str,
  validator: &jsonschema::Validator,
  args: &serde_json::Value,
) -> Result<(), String> {
  let Some(messages) = schema_violations(validator, args) else {
    return Ok(());
  };
  Err(format!(
    "invalid arguments for `{extension}` (does not match inputSchema):\n- {messages}"
  ))
}

/// Declare the reserved `session` routing key on a promoted tool's advertised
/// schema, so the browser it acts in is selectable from `tools/list` rather
/// than only from reading the dispatch. An extension that declares its own
/// `session` keeps it.
fn advertise_session(
  schema: Arc<serde_json::Map<String, serde_json::Value>>,
) -> Arc<serde_json::Map<String, serde_json::Value>> {
  let Some(session) = crate::params::session_property_schema() else {
    return schema;
  };
  let mut out = (*schema).clone();
  {
    let properties = out
      .entry("properties")
      .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    match properties.as_object_mut() {
      Some(properties) if !properties.contains_key("session") => {
        properties.insert("session".to_string(), session);
      },
      // Already declared, or a `properties` this cannot extend: leave it be.
      _ => return schema,
    }
  }
  out
    .entry("type")
    .or_insert_with(|| serde_json::Value::String("object".to_string()));
  Arc::new(out)
}
