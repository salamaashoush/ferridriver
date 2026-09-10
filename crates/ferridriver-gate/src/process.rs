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
    command
      .args(["--port", "47831", "--static", "tests/assets"])
      .current_dir(root)
      .env("TOKIO_WORKER_THREADS", runtime_workers())
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(File::create(logs.join("fixtures.log"))?)
      .kill_on_drop(true);
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

fn runtime_workers() -> std::ffi::OsString {
  std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| "2".into())
}

impl ProcessGroup {
  fn signal(&self, signal: i32) {
    if let Ok(pid) = i32::try_from(self.0) {
      // This PID belongs to the process-group leader spawned by this job.
      #[allow(unsafe_code)]
      unsafe {
        libc::kill(-pid, signal);
      }
    }
  }

  async fn terminate(&self, child: &mut tokio::process::Child) {
    self.signal(libc::SIGTERM);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    self.signal(libc::SIGKILL);
  }
}

impl Drop for ProcessGroup {
  fn drop(&mut self) {
    if let Ok(pid) = i32::try_from(self.0) {
      // The child is a new process-group leader; this cannot signal the gate's
      // own group. Drop also runs when a timeout or cancellation drops wait().
      #[allow(unsafe_code)]
      unsafe {
        libc::kill(-pid, libc::SIGKILL);
      }
    }
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
    .env("TOKIO_WORKER_THREADS", runtime_workers())
    .env("FERRITEST_HEADLESS", "true")
    .env_remove("DISPLAY")
    .env_remove("WAYLAND_DISPLAY")
    .stdin(Stdio::null())
    .stdout(file.try_clone()?)
    .stderr(file)
    .kill_on_drop(true);
  command.as_std_mut().process_group(0);
  let started = Instant::now();
  let mut child = command.spawn().with_context(|| format!("start {}", job.name))?;
  let group = ProcessGroup(child.id().context("child has no PID")?);
  let passed = tokio::select! {
    result = child.wait() => result?.success(),
    () = tokio::time::sleep(Duration::from_secs(job.timeout)) => {
      eprintln!("{} exceeded {}s", job.name, job.timeout);
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

use std::os::unix::process::CommandExt;
