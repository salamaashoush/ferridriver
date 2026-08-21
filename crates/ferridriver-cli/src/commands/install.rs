//! `ferridriver install` — download browser builds into the local cache.
//!
//! The progress callback fires once per HTTP chunk, so the previous
//! `eprintln!` per event turned a ~150MB browser download into thousands of
//! scrollback lines and hid whichever step actually failed. Chunks now feed a
//! throttled single-line bar (inert when there is no terminal), and each phase
//! leaves exactly one summary line behind.

use std::sync::Mutex;

use ferridriver::install::{BrowserInstaller, InstallProgress};

use crate::cli;
use crate::ui;

/// The browsers this command knows how to fetch, in the order `--help` lists
/// them.
const KNOWN: [&str; 4] = ["chromium", "chromium-headless-shell", "firefox", "webkit"];

pub async fn run(args: cli::InstallArgs) -> anyhow::Result<()> {
  let mut browsers = args.browsers;
  if browsers.is_empty() {
    browsers.push("chromium".to_string());
  }
  if let Some(rejected) = browsers.iter().find(|b| !KNOWN.contains(&b.as_str())) {
    anyhow::bail!("unknown browser {rejected:?} (expected one of: {})", KNOWN.join(", "));
  }

  let installer = BrowserInstaller::new();
  let mut on_disk: Vec<(String, String)> = Vec::new();

  if args.with_deps {
    let phase = Phase::new("system dependencies");
    installer.install_system_deps(phase.callback()).await?;
    phase.finish();
  }

  for browser in &browsers {
    let phase = Phase::new(browser);
    let outcome = match browser.as_str() {
      "chromium" => installer.install_chromium(phase.callback()).await,
      "chromium-headless-shell" => installer.install_chromium_headless_shell(phase.callback()).await,
      "firefox" => installer.install_firefox(phase.callback()).await,
      "webkit" => installer.install_webkit(phase.callback()).await,
      // Rejected above, before any download started.
      other => unreachable!("unvalidated browser {other:?}"),
    };
    // The bar owns the line the failure has to be readable on, so it closes
    // itself before the error propagates and `main` prints it.
    if let Err(error) = outcome {
      phase.finish_failed();
      return Err(anyhow::anyhow!("installing {browser}: {error}"));
    }
    if let Some(done) = phase.finish() {
      on_disk.push((browser.clone(), done));
    }
  }

  if ui::json() {
    let payload: Vec<serde_json::Value> = on_disk
      .iter()
      .map(|(name, path)| serde_json::json!({ "browser": name, "path": path }))
      .collect();
    return ui::print_json(&payload);
  }
  ui::next_steps(&[
    ("check the setup", "ferridriver doctor".to_string()),
    ("run a suite", "ferridriver test".to_string()),
  ]);
  Ok(())
}

/// What a phase turned out to have done, read back once it ends.
enum Outcome {
  /// System packages went in; there is no path to report.
  Deps,
  /// A browser is on disk at `path`, either fetched now or already there.
  Browser { version: String, path: String },
}

/// One installer step, and the progress line it owns.
///
/// The installer takes `impl Fn(InstallProgress)` — a shared, non-mut
/// callback it may call from anywhere in the download — so the bar's mutable
/// state sits behind a mutex rather than being threaded through as `&mut`.
struct Phase {
  bar: Mutex<ui::Progress>,
  outcome: Mutex<Option<Outcome>>,
  label: String,
}

impl Phase {
  fn new(label: &str) -> Self {
    Self {
      bar: Mutex::new(ui::Progress::new(label.to_string())),
      outcome: Mutex::new(None),
      label: label.to_string(),
    }
  }

  fn callback(&self) -> impl Fn(InstallProgress) + '_ {
    move |event| {
      let Ok(mut bar) = self.bar.lock() else { return };
      match event {
        InstallProgress::Resolving => bar.tick("resolving the latest version"),
        InstallProgress::Downloading {
          bytes_downloaded,
          total_bytes,
        } => bar.set(bytes_downloaded, total_bytes),
        InstallProgress::Extracting => bar.tick("extracting"),
        InstallProgress::InstallingDeps { distro } => bar.tick(&format!("installing packages ({distro})")),
        InstallProgress::DepsInstalled => {
          if let Ok(mut outcome) = self.outcome.lock() {
            *outcome = Some(Outcome::Deps);
          }
        },
        InstallProgress::Complete { version, path } | InstallProgress::AlreadyInstalled { version, path } => {
          if let Ok(mut outcome) = self.outcome.lock() {
            *outcome = Some(Outcome::Browser { version, path });
          }
        },
      }
    }
  }

  /// Close the line as a failure, leaving whatever the bar had drawn erased
  /// so the error lands on a clean line.
  fn finish_failed(self) {
    if let Ok(bar) = self.bar.into_inner() {
      bar.finish_fail(&format!("{} failed", self.label));
    }
  }

  /// Close the line, and return where the browser landed when this phase
  /// installed one.
  fn finish(self) -> Option<String> {
    let bar = self.bar.into_inner().ok()?;
    match self.outcome.into_inner().ok().flatten() {
      Some(Outcome::Deps) => {
        bar.finish_ok(&format!("{} installed", self.label));
        None
      },
      Some(Outcome::Browser { version, path }) => {
        bar.finish_ok(&format!(
          "{} {version} {}",
          self.label,
          ui::dim(&ui::short_path(std::path::Path::new(&path), 60))
        ));
        Some(path)
      },
      None => {
        bar.finish_quiet(&format!("{}: nothing to do", self.label));
        None
      },
    }
  }
}
