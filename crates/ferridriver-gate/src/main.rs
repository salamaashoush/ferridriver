mod process;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

#[derive(Clone)]
struct Job {
  name: String,
  command: Vec<String>,
  cwd: PathBuf,
  env: BTreeMap<String, String>,
  dependencies: Vec<String>,
  browsers: usize,
  cargo: bool,
  timeout: u64,
}

impl Job {
  fn new(name: &str, command: &[&str], dependencies: &[&str]) -> Self {
    Self {
      name: name.into(),
      command: command.iter().map(|s| (*s).into()).collect(),
      cwd: PathBuf::new(),
      env: BTreeMap::new(),
      dependencies: dependencies.iter().map(|s| (*s).into()).collect(),
      browsers: 0,
      cargo: command.first() == Some(&"cargo"),
      timeout: 900,
    }
  }
}

fn positive(value: &str) -> Result<usize> {
  let n = value.parse::<usize>().context("expected a positive worker count")?;
  if n == 0 {
    bail!("worker count must be greater than zero");
  }
  Ok(n)
}

fn select_jobs(jobs: Vec<Job>, selected: &[String]) -> Result<Vec<Job>> {
  if selected.is_empty() {
    return Ok(jobs);
  }
  let mut required = BTreeSet::new();
  let mut pending = selected.to_vec();
  while let Some(name) = pending.pop() {
    if name == "napi" {
      let suites: Vec<_> = jobs.iter().filter(|job| job.name.starts_with("napi/")).collect();
      if suites.is_empty() {
        bail!("no addon test files found");
      }
      pending.extend(suites.into_iter().map(|job| job.name.clone()));
      continue;
    }
    let job = jobs
      .iter()
      .find(|job| job.name == name)
      .with_context(|| format!("unknown check: {name}"))?;
    if required.insert(name) {
      pending.extend(job.dependencies.iter().cloned());
    }
  }
  Ok(jobs.into_iter().filter(|job| required.contains(&job.name)).collect())
}

fn jobs(root: &Path, workers: usize, ready: bool) -> Result<Vec<Job>> {
  let mut jobs = vec![
    Job::new(
      "build",
      &["cargo", "build", "--locked", "--workspace", "--bins", "--lib"],
      if ready { &["lint"] } else { &[] },
    ),
    Job::new(
      "rust-build",
      &[
        "cargo",
        "test",
        "--locked",
        "--workspace",
        "--no-run",
        "--message-format=json",
      ],
      &["build"],
    ),
    Job::new(
      "types",
      &[
        "bun",
        "x",
        "--package",
        "typescript",
        "tsc",
        "--noEmit",
        "-p",
        "tests/tsconfig.json",
      ],
      &[],
    ),
    Job::new("napi-build", &["bun", "run", "build:debug"], &["build"]),
    Job::new(
      "doc-tests",
      &["cargo", "test", "--locked", "--workspace", "--doc"],
      &["rust-build"],
    ),
  ];
  let mut addon_files = std::fs::read_dir(root.join("crates/ferridriver-node/test"))?
    .map(|entry| entry.map(|entry| entry.path()))
    .collect::<std::io::Result<Vec<_>>>()?;
  addon_files.sort();
  for path in addon_files {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    if path.is_file()
      && [".test.ts", ".test.js", ".test.mjs"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
      jobs.push(Job::new(
        &format!("napi/{name}"),
        &["bun", "test", &format!("test/{name}")],
        &["napi-build", "types"],
      ));
    }
  }
  if ready {
    jobs.extend(ready_jobs());
  }
  jobs.extend(suite_jobs(workers));
  for job in &mut jobs {
    if job.name.starts_with("napi") {
      job.cwd = "crates/ferridriver-node".into();
      if job.name == "napi-build" {
        job.cargo = true;
      } else {
        job.browsers = workers.min(2);
      }
    }
    if job.name == "format" {
      job.cargo = false;
    }
  }
  Ok(jobs)
}

fn ready_jobs() -> Vec<Job> {
  let mut jobs = Vec::new();
  jobs.push(Job::new("format", &["cargo", "fmt", "--all", "--", "--check"], &[]));
  jobs.push(Job::new(
    "lint",
    &[
      "cargo",
      "clippy",
      "--locked",
      "--workspace",
      "--all-targets",
      "--",
      "-D",
      "warnings",
    ],
    &["format"],
  ));
  let mut docs = Job::new(
    "docs",
    &["cargo", "doc", "--locked", "--workspace", "--no-deps", "--keep-going"],
    &["lint"],
  );
  docs.env.insert(
    "RUSTDOCFLAGS".into(),
    format!("{} -Dwarnings", std::env::var("RUSTDOCFLAGS").unwrap_or_default()),
  );
  jobs.push(docs);
  jobs
}

fn suite_jobs(workers: usize) -> Vec<Job> {
  let mut jobs = Vec::new();
  let e2e_workers = workers.div_ceil(2);
  let shared_workers = (workers / 4).max(1);
  let worker_count = shared_workers.to_string();
  let mut e2e = Job::new(
    "e2e",
    &[
      "target/debug/ferridriver",
      "test",
      "--headless",
      "--workers",
      &e2e_workers.to_string(),
      "--forbid-only",
      "--retries",
      "0",
    ],
    &["build", "types"],
  );
  e2e.browsers = e2e_workers;
  jobs.push(e2e);
  for (name, command) in [
    (
      "integration",
      vec![
        "target/debug/ferridriver",
        "test",
        "--no-inherit",
        "--config",
        "tests/integration/ferridriver.toml",
        "--headless",
        "--workers",
        &worker_count,
      ],
    ),
    (
      "bdd",
      vec![
        "target/debug/ferridriver",
        "bdd",
        "tests/features/",
        "--headless",
        "--workers",
        &worker_count,
      ],
    ),
    (
      "acceptance",
      vec![
        "target/debug/ferridriver",
        "test",
        "--no-inherit",
        "--config",
        "tests/acceptance/parity/playwright.config.ts",
        "--headless",
        "--workers",
        &worker_count,
      ],
    ),
    (
      "acceptance-bdd",
      vec![
        "../../../target/debug/ferridriver",
        "bdd",
        "--no-inherit",
        "--headless",
        "--workers",
        &worker_count,
      ],
    ),
  ] {
    let mut job = Job::new(name, &command, &["build", "types"]);
    if matches!(name, "integration" | "acceptance") {
      job
        .command
        .extend(["--forbid-only", "--retries", "0"].map(String::from));
    }
    job.browsers = shared_workers;
    if name == "acceptance-bdd" {
      job.cwd = "tests/acceptance/driving".into();
    }
    jobs.push(job);
  }
  jobs
}

fn rust_jobs(log: &Path, root: &Path, workers: usize) -> Result<Vec<Job>> {
  let mut jobs = Vec::new();
  let mut seen = BTreeSet::new();
  for line in std::fs::read_to_string(log)?.lines() {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    if value["reason"] != "compiler-artifact" || value["profile"]["test"] != true {
      continue;
    }
    let Some(executable) = value["executable"].as_str() else {
      continue;
    };
    if !seen.insert(executable.to_string()) {
      continue;
    }
    let target = value["target"]["name"]
      .as_str()
      .context("test artifact missing target name")?;
    let source = value["target"]["src_path"]
      .as_str()
      .context("test artifact missing source")?;
    let crate_root = Path::new(source)
      .ancestors()
      .find(|dir| dir.join("Cargo.toml").exists())
      .context("test crate root")?;
    let package = crate_root
      .file_name()
      .context("test crate directory")?
      .to_string_lossy();
    let integration = value["target"]["kind"]
      .as_array()
      .is_some_and(|kinds| kinds.iter().any(|kind| kind == "test"));
    let mut job = Job::new(&format!("rust/{package}/{target}"), &[executable], &["rust-build"]);
    job.cargo = package == "ferridriver-cli" && target == "test_ui_mode";
    let example = source.contains("/examples/");
    if integration && !example {
      job.command.push("--test-threads=1".into());
    } else if !integration {
      job.command.push(format!("--test-threads={workers}"));
    }
    job.cwd = crate_root.strip_prefix(root)?.into();
    job.browsers = if !integration {
      0
    } else if package == "ferridriver-test" && target != "screenshot_diff" {
      workers
    } else {
      1
    };
    job.env.insert("FERRITEST_WORKERS".into(), "1".into());
    job.env.insert("FERRITEST_HEADLESS".into(), "true".into());
    jobs.push(job);
  }
  if jobs.is_empty() {
    bail!("cargo produced no test executables");
  }
  Ok(jobs)
}

#[tokio::main]
async fn main() -> Result<()> {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
  let cpus = std::thread::available_parallelism()?.get();
  let mut workers = std::env::var("FERRIDRIVER_GATE_WORKERS")
    .ok()
    .map(|s| positive(&s))
    .transpose()?
    .unwrap_or(cpus);
  let mut parallel = cpus;
  let mut mode = "ready".to_string();
  let mut selected = Vec::new();
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "ready" | "test" => mode = arg,
      "--workers" => workers = positive(&args.next().context("--workers needs a value")?)?,
      "--jobs" | "-j" => parallel = positive(&args.next().context("--jobs needs a value")?)?,
      "--only" => selected.push(args.next().context("--only needs a check name")?),
      _ => bail!("unknown argument {arg}; usage: cargo gate [ready|test] [--workers N] [--jobs N] [--only NAME]"),
    }
  }
  let jobs = select_jobs(jobs(&root, workers, mode == "ready")?, &selected)?;
  run_gate(&root, workers, parallel, jobs).await
}

fn log_paths(root: &Path) -> Result<(PathBuf, PathBuf)> {
  let cache = std::env::var_os("FERRIDRIVER_GATE_CACHE_DIR").map_or_else(|| root.join("target/gate"), PathBuf::from);
  let logs = cache.join(format!(
    "{}-{}",
    SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    std::process::id()
  ));
  std::fs::create_dir_all(&logs)?;
  Ok((logs, cache.join("timings.json")))
}

fn sort_jobs(pending: &mut [Job], timings: &BTreeMap<String, f64>) {
  let prerequisites: BTreeSet<_> = pending
    .iter()
    .flat_map(|job| job.dependencies.iter().cloned())
    .collect();
  pending.sort_by(|a, b| {
    prerequisites
      .contains(&b.name)
      .cmp(&prerequisites.contains(&a.name))
      .then_with(|| {
        timings
          .get(&b.name)
          .unwrap_or(&f64::MAX)
          .total_cmp(timings.get(&a.name).unwrap_or(&f64::MAX))
      })
  });
}

async fn run_gate(root: &Path, workers: usize, parallel: usize, mut pending: Vec<Job>) -> Result<()> {
  let (logs, timings_path) = log_paths(root)?;
  let mut timings: BTreeMap<String, f64> = std::fs::read(&timings_path)
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default();
  let mut completed = BTreeMap::<String, bool>::new();
  let mut running = tokio::task::JoinSet::new();
  let (cancel, cancellation) = tokio::sync::watch::channel(false);
  let mut used_browsers = 0;
  let mut cargo_busy = false;
  let mut fixture_server = None;
  let started = Instant::now();
  eprintln!(
    "Gate: {parallel} jobs, {workers} browser slots. Logs: {}",
    logs.display()
  );
  loop {
    sort_jobs(&mut pending, &timings);
    block_failed_jobs(&mut pending, &mut completed);
    let mut index = 0;
    while index < pending.len() {
      let job = &pending[index];
      if running.len() >= parallel
        || (job.cargo && cargo_busy)
        || used_browsers + job.browsers > workers
        || !job.dependencies.iter().all(|dep| completed.get(dep) == Some(&true))
      {
        index += 1;
        continue;
      }
      let mut job = pending.remove(index);
      if matches!(job.name.as_str(), "e2e" | "bdd") && fixture_server.is_none() {
        fixture_server = Some(process::FixtureServer::start(root, &logs).await?);
      }
      job.env.insert(
        "FERRIDRIVER_BIN".into(),
        root.join("target/debug/ferridriver").display().to_string(),
      );
      used_browsers += job.browsers;
      cargo_busy |= job.cargo;
      let root = root.to_path_buf();
      let logs = logs.clone();
      let cancellation = cancellation.clone();
      eprintln!("START {}", job.name);
      running.spawn(async move {
        let result = process::run(&job, &root, &logs, cancellation).await;
        (job, result)
      });
    }
    if running.is_empty() {
      if pending.is_empty() {
        break;
      }
      bail!("gate dependency cycle or unschedulable resource request");
    }
    let (job, result) = tokio::select! {
      result = running.join_next() => result.context("missing running job")??,
      _ = tokio::signal::ctrl_c() => {
        cancel.send_replace(true);
        while running.join_next().await.is_some() {}
        if let Some(server) = &mut fixture_server {
          server.stop().await;
        }
        std::fs::write(&timings_path, serde_json::to_vec_pretty(&timings)?)?;
        bail!("gate interrupted; child processes stopped");
      }
    };
    used_browsers -= job.browsers;
    if job.cargo {
      cargo_busy = false;
    }
    record_result(job, result, root, workers, &mut pending, &mut completed, &mut timings)?;
  }
  if let Some(server) = &mut fixture_server {
    server.stop().await;
  }
  std::fs::write(&timings_path, serde_json::to_vec_pretty(&timings)?)?;
  let failures = completed.values().filter(|&&passed| !passed).count();
  eprintln!(
    "{} checks, {failures} failed or blocked, {:.2}s total",
    completed.len(),
    started.elapsed().as_secs_f64()
  );
  if failures > 0 {
    bail!("gate failed; full logs: {}", logs.display());
  }
  Ok(())
}

fn block_failed_jobs(pending: &mut Vec<Job>, completed: &mut BTreeMap<String, bool>) {
  loop {
    let before = pending.len();
    pending.retain(|job| {
      if job.dependencies.iter().any(|dep| completed.get(dep) == Some(&false)) {
        eprintln!("BLOCKED {} (dependency failed)", job.name);
        completed.insert(job.name.clone(), false);
        false
      } else {
        true
      }
    });
    if pending.len() == before {
      break;
    }
  }
}

fn record_result(
  job: Job,
  result: Result<process::Finished>,
  root: &Path,
  workers: usize,
  pending: &mut Vec<Job>,
  completed: &mut BTreeMap<String, bool>,
  timings: &mut BTreeMap<String, f64>,
) -> Result<()> {
  match result {
    Ok(result) => {
      eprintln!(
        "{} {} ({:.2}s) {}",
        if result.passed { "PASS" } else { "FAIL" },
        result.name,
        result.elapsed,
        result.log.display()
      );
      if result.passed {
        timings.insert(result.name.clone(), result.elapsed);
        if result.name == "rust-build" {
          pending.extend(rust_jobs(&result.log, root, workers)?);
        }
      } else {
        let text = std::fs::read_to_string(&result.log).unwrap_or_default();
        let lines: Vec<_> = text.lines().collect();
        eprintln!("{}", lines[lines.len().saturating_sub(30)..].join("\n"));
      }
      completed.insert(result.name, result.passed);
    },
    Err(error) => {
      eprintln!("FAIL {}: {error:#}", job.name);
      completed.insert(job.name, false);
    },
  }
  Ok(())
}
