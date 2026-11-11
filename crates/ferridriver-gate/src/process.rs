use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::Job;

pub struct FixtureServer {
  child: Option<tokio::process::Child>,
  group: Option<ProcessGroup>,
}

impl FixtureServer {
  pub async fn start(root: &Path, logs: &Path) -> Result<Self> {
    use tokio::io::AsyncBufReadExt;

    if tokio::net::TcpStream::connect("127.0.0.1:47831").await.is_ok() {
      return Ok(Self {
        child: None,
        group: None,
      });
    }
    let mut command = Command::new(root.join("target/debug/ferridriver-fixtures"));
    // The same server ferridriver.toml's webServer starts: the e2e suite
    // reuses whichever is listening, and its proxy specs need the proxies.
    command
      .args([
        "--port",
        "47831",
        "--auth-proxy-port",
        "47832",
        "--socks-proxy-port",
        "47833",
        "--static",
        "tests/assets",
      ])
      .current_dir(root)
      .env("TOKIO_WORKER_THREADS", runtime_workers(2))
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(File::create(logs.join("fixtures.log"))?)
      .kill_on_drop(true);
    #[cfg(unix)]
    command.as_std_mut().process_group(0);
    let mut child = command.spawn().context("start shared fixture server")?;
    let group = ProcessGroup(child.id().context("fixture server has no PID")?);
    let stdout = child.stdout.take().context("fixture server stdout")?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
      .await
      .context("fixture server did not announce readiness")??
      .context("fixture server exited before readiness")?;
    anyhow::ensure!(
      ready.starts_with("ferridriver-fixtures serving http://127.0.0.1:47831 "),
      "unexpected fixture server readiness: {ready}"
    );
    tokio::spawn(async move { while matches!(lines.next_line().await, Ok(Some(_))) {} });
    Ok(Self {
      child: Some(child),
      group: Some(group),
    })
  }

  pub async fn stop(&mut self) {
    if let (Some(child), Some(group)) = (&mut self.child, &self.group) {
      group.terminate(child).await;
      child.wait().await.ok();
    }
  }
}

struct ProcessGroup(u32);

fn runtime_workers(parallelism: usize) -> std::ffi::OsString {
  // Synchronous JS in one realm must not occupy every executor thread and stall other realms' timers.
  std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| parallelism.max(2).to_string().into())
}

impl ProcessGroup {
  #[cfg(unix)]
  fn signal(&self, signal: i32) {
    if let Ok(pid) = i32::try_from(self.0) {
      // The child is a new process-group leader; this cannot signal the gate's
      // own group.
      #[allow(unsafe_code)]
      unsafe {
        libc::kill(-pid, signal);
      }
    }
  }

  #[cfg(unix)]
  fn kill(&self) {
    self.signal(libc::SIGKILL);
  }

  /// Windows has no process groups to signal; `taskkill /T` walks the tree
  /// from the job's PID, as Playwright's `killProcessTree` does.
  #[cfg(windows)]
  fn kill(&self) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
      .args(["/pid", &self.0.to_string(), "/T", "/F"])
      .creation_flags(CREATE_NO_WINDOW)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .status();
  }

  /// Ask the job to stop, give it two seconds, then kill what is left.
  /// Windows has no stop to ask for, so the tree is killed and the two
  /// seconds go to seeing it gone.
  async fn terminate(&self, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    self.signal(libc::SIGTERM);
    #[cfg(windows)]
    self.kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    #[cfg(unix)]
    self.kill();
  }
}

impl Drop for ProcessGroup {
  fn drop(&mut self) {
    // Also runs when a timeout or cancellation drops wait().
    self.kill();
  }
}

pub struct Finished {
  pub name: String,
  pub passed: bool,
  pub elapsed: f64,
  pub log: PathBuf,
}

pub async fn run(
  job: &Job,
  root: &Path,
  logs: &Path,
  mut cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<Finished> {
  let log = logs.join(format!("{}.log", job.name.replace('/', "-")));
  let file = File::create(&log)?;
  let mut command = Command::new(&job.command[0]);
  // `cargo run` exports its package metadata; dependency build scripts can
  // fingerprint that unrelated package and rebuild on every entry-point switch.
  for (name, _) in std::env::vars_os() {
    if name.to_str().is_some_and(|name| {
      name.starts_with("CARGO_PKG_")
        || matches!(
          name,
          "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH" | "CARGO_MANIFEST_LINKS"
        )
    }) {
      command.env_remove(name);
    }
  }
  command
    .args(&job.command[1..])
    .current_dir(root.join(&job.cwd))
    .envs(&job.env)
    .env("TOKIO_WORKER_THREADS", runtime_workers(job.browsers.max(4)))
    .env("FERRITEST_HEADLESS", "true")
    .env_remove("DISPLAY")
    .env_remove("WAYLAND_DISPLAY")
    .stdin(Stdio::null())
    .stdout(file.try_clone()?)
    .stderr(file)
    .kill_on_drop(true);
  #[cfg(unix)]
  command.as_std_mut().process_group(0);
  let started = Instant::now();
  let mut child = command.spawn().with_context(|| format!("start {}", job.name))?;
  let group = ProcessGroup(child.id().context("child has no PID")?);
  let passed = tokio::select! {
    result = child.wait() => result?.success(),
    () = tokio::time::sleep(Duration::from_secs(job.timeout)) => {
      eprintln!("{} exceeded {}s", job.name, job.timeout);
      #[cfg(target_os = "macos")]
      sample_stacks(group.0, logs, &job.name).await;
      false
    }
    _ = cancellation.changed() => false,
  };
  group.terminate(&mut child).await;
  child.wait().await.ok();
  drop(group);
  Ok(Finished {
    name: job.name.clone(),
    passed,
    elapsed: started.elapsed().as_secs_f64(),
    log,
  })
}

/// Where a job that ran out of time is stuck: every thread's stack, beside
/// its log so CI uploads both. A thread blocked outright is invisible to
/// any timer inside the job; only a sampler from outside can name it.
#[cfg(target_os = "macos")]
async fn sample_stacks(pid: u32, logs: &Path, name: &str) {
  let out = logs.join(format!("{}-stacks.txt", name.replace('/', "-")));
  match Command::new("/usr/bin/sample")
    .args([pid.to_string().as_str(), "2", "-file"])
    .arg(&out)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .await
  {
    Ok(status) if status.success() => eprintln!("{name} stacks: {}", out.display()),
    Ok(status) => eprintln!("{name} stacks: sample exited with {status}"),
    Err(error) => eprintln!("{name} stacks: {error}"),
  }
}

#[cfg(unix)]
use std::os::unix::process::CommandExt;
