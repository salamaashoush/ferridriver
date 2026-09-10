use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use ferridriver::backend::process::{ChildGroup, track_spawned};
use serde_json::{Value, json};

pub async fn run() -> Result<Value> {
  let child = tokio::process::Command::new("cat")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::null())
    .kill_on_drop(true)
    .spawn()?;
  let pid = child.id().context("child PID missing")?;
  let mut child = ChildGroup::recorded(child, None, false);
  let path = dirs::cache_dir()
    .context("cache directory unavailable")?
    .join("ferridriver/procs")
    .join(format!("{}-{pid}.json", std::process::id()));
  let profile = std::path::PathBuf::from("a".repeat(256 * 1024));
  track_spawned(pid, Some(&profile), false);
  let done = Arc::new(AtomicBool::new(false));
  let reader_done = Arc::clone(&done);
  let barrier = Arc::new(std::sync::Barrier::new(2));
  let reader_barrier = Arc::clone(&barrier);
  let reader = std::thread::spawn(move || {
    let mut reads = 0;
    let mut malformed = 0;
    let mut io_errors = 0;
    reader_barrier.wait();
    loop {
      match std::fs::read(&path) {
        Ok(bytes) => {
          reads += 1;
          if serde_json::from_slice::<Value>(&bytes).is_err() {
            malformed += 1;
          }
        },
        Err(_) => io_errors += 1,
      }
      if reader_done.load(Ordering::Acquire) {
        break;
      }
    }
    (reads, malformed, io_errors)
  });
  barrier.wait();
  for _ in 0..64 {
    track_spawned(pid, Some(&profile), false);
  }
  done.store(true, Ordering::Release);
  let observations = reader.join();
  let alive = child.is_running();
  child.shutdown().await;
  let (reads, malformed, io_errors) = observations.map_err(|_| anyhow::anyhow!("record reader panicked"))?;
  Ok(json!({ "reads": reads, "malformed": malformed, "ioErrors": io_errors, "alive": alive, "writes": 64 }))
}
