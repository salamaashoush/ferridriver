//! Binding a configured `[browser]` instance.
//!
//! Shared by `run --instance` and anything else that needs the browser a
//! named instance describes. It goes through the MCP server's own state
//! builder rather than rebuilding the wiring, so `--instance staging` cannot
//! come to mean one thing under `run` and another under `mcp`.

use std::sync::Arc;

/// Launch or attach the browser a configured `[browser]` instance names, and
/// open its first page.
///
/// Goes through [`ferridriver_mcp::server::browser_state_for`] rather than
/// rebuilding the wiring: the instance's overrides, its discover command and
/// the section defaults all have to resolve the same way they do for the MCP
/// server, or `--instance staging` would mean something different depending on
/// which host read it.
pub async fn provision_instance(
  mcp_config: ferridriver_config::mcp::McpConfig,
  instance: &str,
  headed: bool,
) -> anyhow::Result<(
  Arc<ferridriver::Page>,
  Arc<ferridriver::context::ContextRef>,
  Arc<ferridriver::Browser>,
)> {
  let mcp_config: Arc<dyn ferridriver_mcp::server::McpServerConfig> = Arc::new(mcp_config);
  mcp_config
    .instance_health(instance)
    .map_err(|e| anyhow::anyhow!("instance `{instance}`: {e}"))?;

  let overrides = mcp_config
    .instance_overrides(instance)
    .map_err(|e| anyhow::anyhow!("instance `{instance}`: {e}"))?;
  let backend = overrides.backend.unwrap_or(ferridriver::backend::BackendKind::CdpPipe);
  // From the merged overrides, not a constant: `headless` set on the section
  // applies to every instance that does not override it, and a hard-coded
  // default here would silently ignore it. `--headed` is the operator asking
  // to watch this one run, so it wins over both.
  let headless = !headed && overrides.headless.unwrap_or(false);
  let mode = mcp_config
    .resolve_instance(instance)
    .unwrap_or(ferridriver::state::ConnectMode::Launch);

  let mut state = ferridriver_mcp::server::browser_state_for(mode, backend, headless, &mcp_config);
  if headed {
    // The per-instance callback re-asserts the section's `headless`, and it is
    // applied after the base plan -- so clearing it there is the only place
    // `--headed` can win.
    let cfg = Arc::clone(&mcp_config);
    state.set_instance_overrides_fn(Arc::new(move |name: &str| {
      let mut resolved = cfg.instance_overrides(name)?;
      resolved.headless = Some(false);
      Ok(resolved)
    }));
  }
  let browser = ferridriver::Browser::from_state(state);
  let state_arc = Arc::clone(browser.state());

  // The full `instance:context` key, not the bare name: `ContextRef` does not
  // run the async bare-name resolution the MCP server does, so a bare label
  // would be read as a context on `default` and launch the wrong browser.
  let ctx_ref = ferridriver::context::ContextRef::new(state_arc, format!("{instance}:default"));
  let page = ctx_ref
    .new_page()
    .await
    .map_err(|e| anyhow::anyhow!("opening a page on instance `{instance}`: {e}"))?;
  Ok((page, Arc::new(ctx_ref), Arc::new(browser)))
}
