//! `ferridriver install` — download browser builds into the local cache.
//!
//! The progress callback fires once per HTTP chunk, so the previous
//! `eprintln!` per event turned a ~150MB browser download into thousands of
//! scrollback lines and hid whichever step actually failed. Chunks now feed a
//! throttled single-line bar (inert when there is no terminal), and each phase
//! leaves exactly one summary line behind.

use std::sync::Mutex;

use ferridriver::install::{BrowserInstaller, CacheFamily, DependencyGroup, InstallProgress};

use crate::cli;
use crate::ui;

/// What this command knows how to fetch, in the order `--help` lists
/// them. `axe` and `ffmpeg` are not browsers; they are the accessibility
/// engine and the video encoder, and they are here because they are
/// provisioned the same way and to the same place, rather than vendored
/// into the repository or left to the system.
const KNOWN: [(&str, Target); 11] = [
  ("chromium", Target::Chromium),
  ("chromium-headless-shell", Target::ChromiumHeadlessShell),
  ("chromedriver", Target::Chromedriver),
  ("firefox", Target::Firefox),
  ("webkit", Target::Webkit),
  ("safari", Target::Safari),
  ("axe", Target::Axe),
  ("ffmpeg", Target::Ffmpeg),
  ("android", Target::Android),
  ("android-driver", Target::AndroidDriver),
  ("ios", Target::Ios),
];

#[derive(Clone, Copy)]
enum Target {
  Chromium,
  ChromiumHeadlessShell,
  Chromedriver,
  Firefox,
  Webkit,
  Safari,
  Axe,
  Ffmpeg,
  Android,
  AndroidDriver,
  Ios,
}

pub(crate) async fn run(args: cli::InstallArgs) -> anyhow::Result<()> {
  let mut browsers = args.browsers;
  if browsers.is_empty() {
    browsers.push("chromium".to_string());
  }
  let targets = browsers
    .iter()
    .map(|browser| {
      KNOWN
        .iter()
        .find(|(name, _)| name == browser)
        .map(|&(_, target)| target)
        .ok_or_else(|| {
          let known: Vec<&str> = KNOWN.iter().map(|(name, _)| *name).collect();
          anyhow::anyhow!("unknown target {browser:?} (expected one of: {})", known.join(", "))
        })
    })
    .collect::<anyhow::Result<Vec<Target>>>()?;

  let installer = BrowserInstaller::new();
  let mut on_disk: Vec<Installed> = Vec::new();

  if args.with_deps {
    let phase = Phase::new("system dependencies");
    let groups: Vec<DependencyGroup> = browsers.iter().filter_map(|b| DependencyGroup::of_target(b)).collect();
    installer.install_system_deps(&groups, phase.callback()).await?;
    phase.finish();
  }

  for (browser, target) in browsers.iter().zip(targets) {
    let phase = Phase::new(browser);
    let outcome = match target {
      Target::Chromium => installer.install_chromium(phase.callback()).await,
      Target::ChromiumHeadlessShell => installer.install_chromium_headless_shell(phase.callback()).await,
      Target::Chromedriver => installer.install_chromedriver(phase.callback()).await,
      Target::Firefox => installer.install_firefox(phase.callback()).await,
      Target::Webkit => installer.install_webkit(phase.callback()).await,
      Target::Safari => installer.install_safari(phase.callback()).await,
      Target::Ios => installer.install_ios(phase.callback()).await,
      Target::Axe => installer.install_axe_core(phase.callback()).await,
      Target::Ffmpeg => installer.install_ffmpeg(phase.callback()).await,
      Target::AndroidDriver => installer.install_android_driver(phase.callback()).await,
      Target::Android => {
        installer
          .install_android(
            &ferridriver::android::AndroidOptions {
              api_level: args.android_api_level,
              sdk_path: args.android_sdk.clone(),
              accept_licenses: args.accept_licenses,
              ..Default::default()
            },
            phase.callback(),
          )
          .await
      },
    };
    // The bar owns the line the failure has to be readable on, so it closes
    // itself before the error propagates and `main` prints it.
    if let Err(error) = outcome {
      phase.finish_failed();
      return Err(anyhow::anyhow!("installing {browser}: {error}"));
    }
    let Some((version, path)) = phase.finish() else {
      continue;
    };
    // What this version replaced is cleared out only once it is in
    // place, and never when the caller asked to keep it.
    let removed = match CacheFamily::of_target(browser) {
      Some(family) if !args.no_remove => installer
        .remove_older_versions(family, &version)
        .await
        .map_err(|error| anyhow::anyhow!("removing older {browser} versions: {error}"))?,
      _ => Vec::new(),
    };
    for gone in &removed {
      ui::note(&format!(
        "removed {browser} {} {}",
        gone.version,
        ui::dim(&ui::short_path(&gone.path, 60))
      ));
    }
    on_disk.push(Installed {
      browser: browser.clone(),
      path,
      removed: removed.into_iter().map(|gone| gone.version).collect(),
    });
  }

  if ui::json() {
    let payload: Vec<serde_json::Value> = on_disk
      .iter()
      .map(|done| serde_json::json!({ "browser": done.browser, "path": done.path, "removed": done.removed }))
      .collect();
    return ui::print_json(&payload);
  }
  ui::next_steps(&[
    ("check the setup", "ferridriver doctor".to_string()),
    ("run a suite", "ferridriver test".to_string()),
  ]);
  Ok(())
}

/// One target this run left on disk, and the versions it replaced.
struct Installed {
  browser: String,
  path: String,
  removed: Vec<String>,
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

  /// Close the line, and return the version and where it landed when
  /// this phase installed a browser.
  fn finish(self) -> Option<(String, String)> {
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
        Some((version, path))
      },
      None => {
        bar.finish_quiet(&format!("{}: nothing to do", self.label));
        None
      },
    }
  }
}
