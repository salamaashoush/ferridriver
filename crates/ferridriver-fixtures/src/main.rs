//! `ferridriver-fixtures --port 47831 --proxy-port 0 --static tests/assets`
//!
//! Runs the fixture server until killed. Spawned by `ferridriver.toml`'s
//! `webServer` command entry for the repo's own test suites.

use std::path::PathBuf;
use std::process::ExitCode;

use ferridriver_fixtures::{FixtureServer, FixtureServerOptions};

fn parse_args() -> Result<FixtureServerOptions, String> {
  let mut options = FixtureServerOptions::default();
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} requires a value"));
    match arg.as_str() {
      "--port" => {
        options.port = value("--port")?.parse().map_err(|e| format!("--port: {e}"))?;
      },
      "--proxy-port" => {
        options.proxy_port = value("--proxy-port")?
          .parse()
          .map_err(|e| format!("--proxy-port: {e}"))?;
      },
      "--static" => {
        options.static_dir = Some(PathBuf::from(value("--static")?));
      },
      other => {
        return Err(format!(
          "unknown argument {other:?} (expected --port, --proxy-port, --static)"
        ));
      },
    }
  }
  Ok(options)
}

#[tokio::main]
async fn main() -> ExitCode {
  let options = match parse_args() {
    Ok(options) => options,
    Err(e) => {
      eprintln!("ferridriver-fixtures: {e}");
      return ExitCode::FAILURE;
    },
  };
  let server = match FixtureServer::start(options).await {
    Ok(server) => server,
    Err(e) => {
      eprintln!("ferridriver-fixtures: bind failed: {e}");
      return ExitCode::FAILURE;
    },
  };
  println!(
    "ferridriver-fixtures serving {} (proxy {})",
    server.url(),
    server.proxy_url()
  );
  server.run_forever().await;
  ExitCode::SUCCESS
}
