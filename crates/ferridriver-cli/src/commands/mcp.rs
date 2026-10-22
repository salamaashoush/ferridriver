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
  let effective = cli::effective_browser(&args.browser, &config.mcp)?;
  config.mcp.browser.browser = Some(effective.browser);
  if args.browser.backend.is_some() || args.browser.browser.is_some() {
    config.mcp.browser.backend = Some(ferridriver_config::mcp::BackendChoice::parse(effective.backend.name())?);
  }
  let (backend, headless) = (effective.backend, effective.headless);
  let connect_mode = args.browser.connect_mode();

  let caps = ferridriver_script::ScriptCaps::from_scripting(&scripting)
    .with_extension_policy(extension_policy)
    .with_extension_settings(extension_settings);
  let config: Arc<dyn ferridriver_mcp::McpServerConfig> = Arc::new(config);
  let build = || {
    McpServer::with_options(connect_mode.clone(), backend, headless, Arc::clone(&config))
      .with_script_caps(caps.clone())
      .with_sidecars(sidecars.clone())
      .with_test_config(test_config.clone())
  };
  match args.transport.transport {
    // stdio IS the protocol channel: nothing but frames may touch stdout,
    // so the startup line goes to stderr or nowhere at all.
    cli::Transport::Stdio => {
      let mut server = build();
      server.load_extensions(&extensions).await;
      ui::note(&format!(
        "serving MCP over stdio ({} backend, {})",
        format!("{backend:?}").to_lowercase(),
        if headless { "headless" } else { "headed" }
      ));
      Box::pin(ferridriver_mcp::mcp::serve_stdio_with(server)).await
    },
    cli::Transport::Http => {
      let options = ferridriver_mcp::mcp::HttpOptions {
        host: args.transport.host,
        port: args.transport.port,
      };
      // One server, with its own browsers, per token: a client cannot reach
      // another's pages by guessing its session key.
      let mut given = args.transport.tokens.clone();
      if given.is_empty()
        && let Ok(from_env) = std::env::var("FERRIDRIVER_MCP_TOKEN")
      {
        given = from_env
          .split(',')
          .map(str::trim)
          .filter(|token| !token.is_empty())
          .map(str::to_string)
          .collect();
      }
      let tokens: Vec<Option<String>> = if given.is_empty() {
        vec![None]
      } else {
        given.into_iter().map(Some).collect()
      };
      let mut clients = Vec::with_capacity(tokens.len());
      for token in tokens {
        let mut server = build();
        server.load_extensions(&extensions).await;
        clients.push(ferridriver_mcp::mcp::HttpClient { token, server });
      }
      ui::note(&format!(
        "serving MCP over HTTP ({} backend, {}){}",
        format!("{backend:?}").to_lowercase(),
        if headless { "headless" } else { "headed" },
        match clients.len() {
          1 if clients[0].token.is_some() => ", bearer token required".to_string(),
          1 => String::new(),
          n => format!(", {n} clients with a token each"),
        }
      ));
      Box::pin(ferridriver_mcp::mcp::serve_http_clients(clients, options)).await
    },
  }
}
