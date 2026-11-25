//! Start Lightpanda's CDP server. Everything after the WebSocket is the
//! shared CDP backend.

use crate::backend::process::ChildGroup;
use crate::error::{FerriError, Result};

/// What `lightpanda serve` is started with beyond the caller's own
/// arguments.
pub(crate) struct ServeOptions<'a> {
  pub proxy: Option<&'a crate::options::ProxyConfig>,
  /// A persistent profile: Lightpanda keeps no profile directory, only
  /// cookies, which it loads from and saves to a file in it.
  pub user_data_dir: Option<&'a std::path::Path>,
  pub args: &'a [String],
  pub env: &'a rustc_hash::FxHashMap<String, String>,
  pub timeout_ms: u64,
}

/// Spawn `lightpanda serve` on a free loopback port and answer the
/// WebSocket URL it serves CDP on, with the process group that owns it.
///
/// # Errors
/// Returns an error when Lightpanda cannot be spawned, exits before it
/// listens, or does not listen within `timeout_ms`.
pub(crate) async fn serve(binary: &std::path::Path, options: ServeOptions<'_>) -> Result<(String, ChildGroup)> {
  let mut command = tokio::process::Command::new(binary);
  command.args(["serve", "--host", "127.0.0.1", "--port", "0", "--log-format", "logfmt"]);
  if let Some(proxy) = options.proxy {
    command.arg("--http-proxy").arg(proxy_url(proxy));
  }
  if let Some(dir) = options.user_data_dir {
    std::fs::create_dir_all(dir)?;
    let jar = dir.join("cookies.json");
    if jar.is_file() {
      command.arg("--cookie").arg(&jar);
    }
    command.arg("--cookie-jar").arg(&jar);
  }
  // Each browser context is a connection of its own (see
  // `connections.rs`), so the server's connection cap is a context cap;
  // the default of 16 is lower than a suite's open contexts reach.
  if !options.args.iter().any(|arg| arg.starts_with("--cdp-max-connections")) {
    command.args(["--cdp-max-connections", "128"]);
  }
  // `serve` fetches no sub-resources unless told to: no iframe documents,
  // no workers, no stylesheets or images. A page is driven as a browser
  // loads it, so all of them load unless the caller chose its own set.
  if !options.args.iter().any(|arg| arg.starts_with("--load-resources")) {
    for resource in ["iframe", "worker", "stylesheet", "image", "beacon"] {
      command.args(["--load-resources", resource]);
    }
  }
  command.args(options.args);
  // Lightpanda reports usage to its vendor unless told not to. A test run
  // does not opt anyone into that; the caller's own env still decides.
  command.env("LIGHTPANDA_DISABLE_TELEMETRY", "true");
  command
    .envs(options.env)
    .kill_on_drop(true)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());
  #[cfg(unix)]
  crate::backend::process::spawn_in_own_session(command.as_std_mut());
  let mut child = crate::backend::process::spawn_exclusive(|| command.spawn())?;
  let stderr = child.stderr.take();
  let mut group = ChildGroup::recorded(child, options.user_data_dir, false);
  let listening = listening_address(stderr, &mut group);
  let address = if options.timeout_ms == 0 {
    listening.await
  } else {
    tokio::time::timeout(std::time::Duration::from_millis(options.timeout_ms), listening)
      .await
      .unwrap_or_else(|_| {
        Err(FerriError::timeout(
          "waiting for Lightpanda to listen",
          options.timeout_ms,
        ))
      })
  };
  match address {
    Ok(address) => Ok((format!("ws://{address}/"), group)),
    Err(error) => {
      let _ = group.shutdown().await;
      Err(error)
    },
  }
}

/// Read Lightpanda's log until it says where it listens. The reader is
/// its own task, which keeps draining the log afterwards so the server
/// never blocks on a full pipe.
async fn listening_address(stderr: Option<tokio::process::ChildStderr>, group: &mut ChildGroup) -> Result<String> {
  use tokio::io::AsyncBufReadExt;
  let stderr = stderr.ok_or_else(|| FerriError::backend("Lightpanda: no stderr handle"))?;
  let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
  tokio::spawn(async move {
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
      tracing::debug!(target: "ferridriver::browser::stderr", stream = "stderr", "{line}");
      if !lines_tx.is_closed() {
        let _ = lines_tx.send(line);
      }
    }
  });
  let mut said = std::collections::VecDeque::new();
  loop {
    match tokio::time::timeout(std::time::Duration::from_millis(250), lines.recv()).await {
      Ok(Some(line)) => {
        if line.contains("msg=\"server running\"")
          && let Some(address) = logfmt_value(&line, "address")
        {
          return Ok(address.to_owned());
        }
        if said.len() == 20 {
          said.pop_front();
        }
        said.push_back(line);
      },
      Ok(None) | Err(_) => {
        if !group.is_running() {
          let status = group.wait().await?;
          let said: Vec<String> = said.into_iter().collect();
          return Err(FerriError::backend(format!(
            "Lightpanda exited during startup with status: {status}\n{}",
            said.join("\n")
          )));
        }
      },
    }
  }
}

fn logfmt_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
  line
    .split_whitespace()
    .find_map(|field| field.strip_prefix(key)?.strip_prefix('='))
    .map(|value| value.trim_matches('"'))
}

/// Lightpanda takes one proxy URL, with any credentials in it.
fn proxy_url(proxy: &crate::options::ProxyConfig) -> String {
  let origin = proxy.server_origin();
  let Some(user) = proxy.username.as_deref().filter(|user| !user.is_empty()) else {
    return origin;
  };
  let Ok(mut url) = reqwest::Url::parse(&origin) else {
    return origin;
  };
  if url.set_username(user).is_err() || url.set_password(proxy.password.as_deref()).is_err() {
    return origin;
  }
  url.to_string()
}

#[cfg(test)]
mod tests {
  #[test]
  fn reads_the_listening_address_from_the_server_log() {
    let line =
      r#"$time=1795546685405 $scope=note $level=note $msg="server running" address=127.0.0.1:50404 max_connections=16"#;
    assert_eq!(super::logfmt_value(line, "address"), Some("127.0.0.1:50404"));
    assert_eq!(super::logfmt_value(line, "missing"), None);
  }
}
