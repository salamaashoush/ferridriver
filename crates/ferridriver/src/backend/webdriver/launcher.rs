use crate::backend::process::ChildGroup;
use crate::error::{FerriError, Result};

type DriverGroup = std::sync::Arc<tokio::sync::Mutex<Option<ChildGroup>>>;

pub(crate) async fn launch_safari<T, F, Fut>(
  env: &rustc_hash::FxHashMap<String, String>,
  timeout_ms: u64,
  connect: F,
) -> Result<(T, DriverGroup)>
where
  F: FnOnce(String) -> Fut,
  Fut: std::future::Future<Output = Result<T>>,
{
  if !cfg!(target_os = "macos") {
    return Err(FerriError::unsupported(
      "Local Safari requires macOS; connect to a remote safaridriver endpoint on other platforms",
    ));
  }
  let driver = Driver {
    program: std::path::Path::new("/usr/bin/safaridriver"),
    args: Vec::new(),
    port_flag: "-p",
    product: "safaridriver",
    startup_hint: "Enable Safari remote automation and sign into the macOS desktop",
    profile: None,
  };
  launch(driver, env, timeout_ms, connect).await
}

/// Start the `WebDriver` service Ladybird ships beside its browser. The
/// service launches Ladybird itself when a session is created, so the
/// browser is a member of the service's process group. Without
/// `user_data_dir` the profile is a temp directory that group owns.
pub(crate) async fn launch_ladybird<T, F, Fut>(
  webdriver: &std::path::Path,
  headless: bool,
  user_data_dir: Option<&std::path::Path>,
  env: &rustc_hash::FxHashMap<String, String>,
  timeout_ms: u64,
  connect: F,
) -> Result<(T, DriverGroup)>
where
  F: FnOnce(String) -> Fut,
  Fut: std::future::Future<Output = Result<T>>,
{
  let (profile, owned) = match user_data_dir {
    // Ladybird takes only an absolute `--profile-path`.
    Some(dir) => {
      std::fs::create_dir_all(dir)?;
      (std::path::absolute(dir)?, false)
    },
    None => (
      tempfile::Builder::new()
        .prefix("ferridriver-ladybird-")
        .tempdir()?
        .keep(),
      true,
    ),
  };
  let mut args: Vec<std::ffi::OsString> = vec!["--listen-address".into(), "127.0.0.1".into()];
  if headless {
    args.push("--headless".into());
  }
  args.push("--profile-path".into());
  args.push(profile.clone().into_os_string());
  let driver = Driver {
    program: webdriver,
    args,
    port_flag: "--port",
    product: "Ladybird WebDriver",
    startup_hint: "Check that this Ladybird build starts on this host",
    profile: Some(DriverProfile { path: &profile, owned }),
  };
  let result = launch(driver, env, timeout_ms, connect).await;
  if result.is_err() && owned {
    let _ = std::fs::remove_dir_all(&profile);
  }
  result
}

/// The `WebDriver` service of the Ladybird whose browser executable is
/// `browser`: beside it in the macOS app bundle, in `libexec` beside
/// `bin` on Linux.
///
/// # Errors
///
/// Returns a backend error when neither location holds the service.
pub(crate) fn ladybird_webdriver(browser: &std::path::Path) -> Result<std::path::PathBuf> {
  let name = if cfg!(windows) { "WebDriver.exe" } else { "WebDriver" };
  let dir = browser.parent().unwrap_or_else(|| std::path::Path::new("."));
  [dir.join(name), dir.join("../libexec").join(name)]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| {
      FerriError::backend(format!(
        "no Ladybird WebDriver service beside {}; the build must include WebDriver",
        browser.display()
      ))
    })
}

/// Where a launched browser keeps its profile, and whether the launch
/// created it and so removes it with the driver.
struct DriverProfile<'a> {
  path: &'a std::path::Path,
  owned: bool,
}

struct Driver<'a> {
  program: &'a std::path::Path,
  args: Vec<std::ffi::OsString>,
  port_flag: &'static str,
  product: &'static str,
  startup_hint: &'static str,
  profile: Option<DriverProfile<'a>>,
}

async fn launch<T, F, Fut>(
  driver: Driver<'_>,
  env: &rustc_hash::FxHashMap<String, String>,
  timeout_ms: u64,
  connect: F,
) -> Result<(T, DriverGroup)>
where
  F: FnOnce(String) -> Fut,
  Fut: std::future::Future<Output = Result<T>>,
{
  let http_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let http_port = http_listener.local_addr()?.port();
  let mut command = tokio::process::Command::new(driver.program);
  command
    .arg(driver.port_flag)
    .arg(http_port.to_string())
    .args(&driver.args)
    .envs(env)
    .kill_on_drop(true)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
  drop(http_listener);
  let mut child = crate::backend::process::spawn_exclusive(|| command.spawn())?;
  let stderr = crate::backend::process::drain_child_stderr(&mut child);
  let group = match driver.profile {
    Some(profile) => ChildGroup::recorded(child, Some(profile.path), profile.owned),
    None => ChildGroup::new(child),
  };
  let group = std::sync::Arc::new(tokio::sync::Mutex::new(Some(group)));
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
          "{} exited during startup: {}. {}",
          driver.product,
          stderr.as_error_context().unwrap_or_default(),
          driver.startup_hint
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
      .unwrap_or_else(|_| Err(FerriError::timeout(format!("launching {}", driver.product), timeout_ms)))
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
            "{error}; {} cleanup failed: {cleanup}",
            driver.product
          )));
        }
        *group = None;
      }
      Err(error)
    },
  }
}
