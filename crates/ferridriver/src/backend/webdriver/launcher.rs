use crate::backend::process::ChildGroup;
use crate::error::{FerriError, Result};

pub(crate) async fn launch_safari<T, F, Fut>(
  env: &rustc_hash::FxHashMap<String, String>,
  timeout_ms: u64,
  connect: F,
) -> Result<(T, std::sync::Arc<tokio::sync::Mutex<Option<ChildGroup>>>)>
where
  F: FnOnce(String) -> Fut,
  Fut: std::future::Future<Output = Result<T>>,
{
  if !cfg!(target_os = "macos") {
    return Err(FerriError::unsupported(
      "Local Safari requires macOS; connect to a remote safaridriver endpoint on other platforms",
    ));
  }
  let http_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let http_port = http_listener.local_addr()?.port();
  let mut command = tokio::process::Command::new("/usr/bin/safaridriver");
  command
    .args(["-p", &http_port.to_string()])
    .envs(env)
    .kill_on_drop(true)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
  drop(http_listener);
  let mut child = command.spawn()?;
  let stderr = crate::backend::process::drain_child_stderr(&mut child);
  let group = std::sync::Arc::new(tokio::sync::Mutex::new(Some(ChildGroup::new(child))));
  crate::state::allocation::retain_driver(&group);
  let launch = async {
    let client = reqwest::Client::builder()
      .no_proxy()
      .build()
      .map_err(|error| FerriError::backend(error.to_string()))?;
    let endpoint = format!("http://127.0.0.1:{http_port}");
    loop {
      if group.lock().await.as_mut().is_none_or(|group| !group.is_running()) {
        return Err(FerriError::backend(format!(
          "safaridriver exited during startup: {}. Enable Safari remote automation and sign into the macOS desktop",
          stderr.as_error_context().unwrap_or_default()
        )));
      }
      if let Ok(response) = client.get(format!("{endpoint}/status")).send().await
        && response.status().is_success()
      {
        break;
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    connect(endpoint).await
  };
  let result = if timeout_ms == 0 {
    launch.await
  } else {
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), launch)
      .await
      .unwrap_or_else(|_| Err(FerriError::timeout("launching Safari", timeout_ms)))
  };
  match result {
    Ok(browser) => Ok((browser, group)),
    Err(error) => {
      if !crate::state::allocation::has_pending_sessions() {
        let mut group = group.lock().await;
        if let Some(child) = group.as_mut()
          && let Err(cleanup) = child.shutdown().await
        {
          return Err(FerriError::backend(format!(
            "{error}; Safari driver cleanup failed: {cleanup}"
          )));
        }
        *group = None;
      }
      Err(error)
    },
  }
}
