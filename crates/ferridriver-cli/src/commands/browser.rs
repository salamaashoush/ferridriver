//! `ferridriver browser serve` — a launched Chromium another process drives.

use std::io::Write as _;

use crate::cli;
use crate::ui;

pub async fn run(args: cli::BrowserCommandArgs) -> anyhow::Result<()> {
  match args.command {
    cli::BrowserCommand::Serve(serve) => Box::pin(serve_browser(serve)).await,
  }
}

async fn serve_browser(args: cli::BrowserServeArgs) -> anyhow::Result<()> {
  // A token-less server on the network is a browser anyone on it can drive.
  let token = args
    .token
    .or_else(|| {
      std::env::var("FERRIDRIVER_BROWSER_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
    })
    .or_else(|| (!args.host.is_loopback()).then(ferridriver::browser_server::random_secret));
  let options = ferridriver::browser_server::LaunchServerOptions {
    launch: ferridriver::options::LaunchOptions {
      headless: Some(!args.headed),
      executable_path: args.executable_path,
      args: args.args,
      ..Default::default()
    },
    serve: ferridriver::browser_server::ServeOptions {
      host: Some(args.host.to_string()),
      port: Some(args.port),
      ws_path: args.ws_path,
      token,
    },
  };
  let server = ferridriver::BrowserType::chromium().launch_server(options).await?;

  // The endpoint is the command's output, so it goes to stdout alone and is
  // flushed at once: a supervisor reads the first line and hands it on.
  let mut stdout = std::io::stdout();
  writeln!(stdout, "{}", server.ws_endpoint())?;
  stdout.flush()?;
  ui::note(&format!("serving Chromium on {}", server.local_addr()));

  let reason = tokio::select! {
    signal = terminate_signal() => signal,
    _ = server.disconnected() => "browser exited",
  };
  ui::note(&format!("{reason}; closing the browser"));
  server.close().await?;
  Ok(())
}

#[cfg(unix)]
async fn terminate_signal() -> &'static str {
  use tokio::signal::unix::{SignalKind, signal};
  let (Ok(mut term), Ok(mut hup)) = (signal(SignalKind::terminate()), signal(SignalKind::hangup())) else {
    let _ = tokio::signal::ctrl_c().await;
    return "SIGINT";
  };
  tokio::select! {
    _ = term.recv() => "SIGTERM",
    _ = hup.recv() => "SIGHUP",
    _ = tokio::signal::ctrl_c() => "SIGINT",
  }
}

#[cfg(not(unix))]
async fn terminate_signal() -> &'static str {
  let _ = tokio::signal::ctrl_c().await;
  "interrupt"
}
