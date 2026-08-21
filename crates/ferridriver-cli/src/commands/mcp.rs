//! `ferridriver mcp` — serve the browser to a coding agent.

use std::sync::Arc;

use ferridriver_config::FerridriverConfig;
use ferridriver_mcp::McpServer;

use crate::cli;
use crate::commands::script_setup;
use crate::ui;

pub async fn run(mut config: FerridriverConfig, args: cli::McpArgs) -> anyhow::Result<()> {
  // The mcp section drives chrome args, instances, and server metadata.
  // CLI flags fall back when the [mcp] section is empty so the user can
  // launch the server with no config file at all.
  let sidecars = script_setup::sidecar_specs(&config);
  let extensions = config.extension_specs();
  let extension_policy = config.extensions.policy();
  let extension_settings = config.extensions.settings();
  let test_config = config.test.clone();
  let scripting = std::mem::take(&mut config.scripting);
  // CLI flags beat the config file. The old order was inverted, so
  // `--headless` / `--backend` were silently dropped whenever the
  // config set those keys at all.
  let effective = cli::effective_browser(&args.browser, &config.mcp);
  let (backend, headless) = (effective.backend, effective.headless);
  let connect_mode = args.browser.connect_mode();

  let caps =
    ferridriver_script::ScriptCaps::resolve_with_commands(&scripting.allow_env, scripting.allow.commands.clone())
      .with_extension_policy(extension_policy)
      .with_extension_settings(extension_settings);
  let mut server = McpServer::with_options(connect_mode, backend, headless, Arc::new(config))
    .with_script_caps(caps)
    .with_sidecars(sidecars)
    .with_test_config(test_config);
  server.load_extensions(&extensions).await;
  match args.transport.transport {
    // stdio IS the protocol channel: nothing but frames may touch stdout,
    // so the startup line goes to stderr or nowhere at all.
    cli::Transport::Stdio => {
      ui::note(&format!(
        "serving MCP over stdio ({} backend, {})",
        format!("{backend:?}").to_lowercase(),
        if headless { "headless" } else { "headed" }
      ));
      Box::pin(ferridriver_mcp::mcp::serve_stdio_with(server)).await
    },
    cli::Transport::Http => {
      let port = args.transport.port;
      ui::note(&format!(
        "serving MCP on {}",
        ui::url(&format!("http://127.0.0.1:{port}/mcp"))
      ));
      Box::pin(ferridriver_mcp::mcp::serve_http_with(server, port)).await
    },
  }
}
