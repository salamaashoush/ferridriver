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
  let proxy = args
    .proxy
    .or_else(|| {
      std::env::var("FERRIDRIVER_BROWSER_PROXY")
        .ok()
        .filter(|proxy| !proxy.is_empty())
    })
    .map(|url| proxy_from_url(&url, args.proxy_bypass))
    .transpose()?;
  let options = ferridriver::browser_server::LaunchServerOptions {
    launch: ferridriver::options::LaunchOptions {
      headless: Some(!args.headed),
      executable_path: args.executable_path,
      args: args.args,
      proxy,
      ..Default::default()
    },
    serve: ferridriver::browser_server::ServeOptions {
      host: Some(args.host.to_string()),
      port: Some(args.port),
      ws_path: args.ws_path,
      token,
    },
  };
  let server = Box::pin(ferridriver::BrowserType::chromium().launch_server(options)).await?;

  // The endpoint is the command's output, so it goes to stdout alone and is
  // flushed at once: a supervisor reads the first line and hands it on.
  let mut stdout = std::io::stdout();
  writeln!(stdout, "{}", server.ws_endpoint())?;
  stdout.flush()?;
  ui::note(&format!("serving Chromium on {}", server.local_addr()));

  let stdin = if args.exit_with_stdin {
    futures::future::Either::Left(super::lifetime::stdin_closed()?)
  } else {
    futures::future::Either::Right(std::future::pending())
  };
  let reason = tokio::select! {
    signal = terminate_signal() => signal,
    _ = server.disconnected() => "browser exited",
    () = stdin => "standard input closed",
  };
  ui::note(&format!("{reason}; closing the browser"));
  server.close().await?;
  Ok(())
}

/// Split credentials out of a proxy URL: browsers take the server alone and
/// answer the proxy's challenge with the rest.
fn proxy_from_url(url: &str, bypass: Option<String>) -> anyhow::Result<ferridriver::options::ProxyConfig> {
  let written = if url.contains("://") {
    url.to_string()
  } else {
    format!("http://{url}")
  };
  let mut parsed = reqwest::Url::parse(&written).map_err(|e| anyhow::anyhow!("--proxy {url:?}: {e}"))?;
  let decode = |value: &str| {
    percent_encoding::percent_decode_str(value)
      .decode_utf8_lossy()
      .into_owned()
  };
  let username = Some(decode(parsed.username())).filter(|name| !name.is_empty());
  let password = parsed.password().map(decode);
  if password.is_some() && username.is_none() {
    anyhow::bail!("--proxy {url:?} has a password but no user name");
  }
  let _ = parsed.set_username("");
  let _ = parsed.set_password(None);
  let server = parsed.as_str().trim_end_matches('/').to_string();
  let proxy = ferridriver::options::ProxyConfig {
    server,
    bypass,
    username,
    password,
  };
  proxy.validate()?;
  Ok(proxy)
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

#[cfg(test)]
mod tests {
  use super::proxy_from_url;

  #[test]
  fn credentials_leave_the_proxy_url() {
    let proxy = proxy_from_url(
      "http://acme%40example.com:p%3Ass@127.0.0.1:3128",
      Some("localhost".into()),
    )
    .unwrap();
    assert_eq!(proxy.server, "http://127.0.0.1:3128");
    assert_eq!(proxy.username.as_deref(), Some("acme@example.com"));
    assert_eq!(proxy.password.as_deref(), Some("p:ss"));
    assert_eq!(proxy.bypass.as_deref(), Some("localhost"));

    let socks = proxy_from_url("socks5://acme:secret@[::1]:1080", None).unwrap();
    assert_eq!(socks.server, "socks5://[::1]:1080");
    assert_eq!(socks.username.as_deref(), Some("acme"));

    let bare = proxy_from_url("127.0.0.1:3128", None).unwrap();
    assert_eq!(bare.server, "http://127.0.0.1:3128");
    assert_eq!(bare.username, None);
    assert_eq!(bare.password, None);
  }

  #[test]
  fn a_proxy_no_browser_can_use_is_refused() {
    assert!(proxy_from_url("ftp://127.0.0.1:21", None).is_err());
    assert!(proxy_from_url("http://:secret@127.0.0.1:3128", None).is_err());
    assert!(proxy_from_url("socks4://acme:secret@127.0.0.1:1080", None).is_err());
  }
}
