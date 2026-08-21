//! A per-instance `userDataDir` has to reach EVERY backend's launch, and
//! the directory it names is the caller's — never removed on teardown.
//!
//! The Firefox and `WebKit` launch paths took only `headless` from the
//! instance overrides, so a configured profile directory was accepted,
//! validated, and then silently dropped: every launch got a throwaway
//! profile and the logins the operator asked to keep vanished with the
//! browser. Config-level tests cannot see that — the settings were
//! resolved correctly and discarded one layer lower.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use ferridriver::backend::BackendKind;
use ferridriver::options::{BrowserKind, LaunchPlan};
use ferridriver::state::{BrowserState, ConnectMode};

/// Whether a launched browser wrote anything into `dir`.
fn has_entries(dir: &Path) -> bool {
  std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// Launch `backend` with a caller-owned profile directory, open a page,
/// then close — asserting the directory was used and survives.
async fn persistent_profile_round_trip(backend: BackendKind, kind: BrowserKind) {
  let root = tempfile::tempdir().expect("tempdir");
  let profile = root.path().join("profile");

  let mut state = BrowserState::with_plan(
    ConnectMode::Launch,
    LaunchPlan {
      backend,
      kind,
      headless: true,
      ..Default::default()
    },
  );
  let dir = profile.clone();
  state.set_instance_overrides_fn(std::sync::Arc::new(move |_| {
    Ok(ferridriver::options::InstanceOverrides {
      user_data_dir: Some(dir.display().to_string()),
      ..Default::default()
    })
  }));

  state.ensure_instance("default").await.expect("launch");
  state.open_page("default", "about:blank").await.expect("open page");

  assert!(
    profile.is_dir(),
    "{backend:?} must launch into the configured profile directory"
  );
  assert!(
    has_entries(&profile),
    "{backend:?} must actually write its profile there: {}",
    profile.display()
  );

  state.shutdown().await;

  // The caller owns this directory. Removing it on teardown would throw
  // away exactly the cookies and logins a persistent profile exists for.
  assert!(
    profile.is_dir() && has_entries(&profile),
    "{backend:?} must not remove a profile it does not own"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn cdp_pipe_uses_and_keeps_a_configured_profile() {
  persistent_profile_round_trip(BackendKind::CdpPipe, BrowserKind::Chromium).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bidi_uses_and_keeps_a_configured_profile() {
  persistent_profile_round_trip(BackendKind::Bidi, BrowserKind::Firefox).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn webkit_uses_and_keeps_a_configured_profile() {
  persistent_profile_round_trip(BackendKind::WebKit, BrowserKind::WebKit).await;
}

/// Without a configured directory the throwaway profile is ferridriver's
/// own, and teardown still removes it — the behaviour the persistent path
/// must not have regressed.
#[tokio::test(flavor = "multi_thread")]
async fn a_throwaway_profile_is_still_cleaned_up() {
  let mut state = BrowserState::with_plan(
    ConnectMode::Launch,
    LaunchPlan {
      backend: BackendKind::Bidi,
      kind: BrowserKind::Firefox,
      headless: true,
      ..Default::default()
    },
  );
  state.ensure_instance("default").await.expect("launch");
  state.open_page("default", "about:blank").await.expect("open page");
  state.shutdown().await;
}

/// A profile Chrome has already saved a window size into must not decide
/// the viewport of the pages ferridriver opens from it.
///
/// Chrome writes `browser.window_placement` into `Default/Preferences`
/// when it exits and restores it on the next launch. A host that
/// emulates no viewport therefore inherits whatever size that profile
/// was last left at — one `--window-size` launch by another tool, or one
/// manual resize, and every later session silently reports it. Playwright
/// never has this problem because it emulates 1280x720 unless told
/// `viewport: null`.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_window_size_does_not_leak_into_the_viewport() {
  let root = tempfile::tempdir().expect("tempdir");
  let profile = root.path().join("profile");
  std::fs::create_dir_all(profile.join("Default")).expect("profile dir");

  // Deliberately unlike the default viewport in BOTH dimensions, so a
  // pass cannot come from one of them coinciding.
  std::fs::write(
    profile.join("Default").join("Preferences"),
    serde_json::json!({
      "browser": {
        "window_placement": {
          "left": 0, "top": 0, "right": 1001, "bottom": 777, "maximized": false,
        }
      }
    })
    .to_string(),
  )
  .expect("seed preferences");

  let mut state = BrowserState::with_plan(
    ConnectMode::Launch,
    LaunchPlan {
      backend: BackendKind::CdpPipe,
      kind: BrowserKind::Chromium,
      headless: true,
      ..Default::default()
    },
  );
  let dir = profile.clone();
  state.set_instance_overrides_fn(std::sync::Arc::new(move |_| {
    Ok(ferridriver::options::InstanceOverrides {
      user_data_dir: Some(dir.display().to_string()),
      ..Default::default()
    })
  }));

  state.ensure_instance("default").await.expect("launch");
  let page = state.open_page("default", "about:blank").await.expect("open page");
  let size = page
    .evaluate("[window.innerWidth, window.innerHeight]")
    .await
    .expect("evaluate")
    .expect("a size");
  state.shutdown().await;

  assert_eq!(
    size,
    serde_json::json!([1280, 720]),
    "the default viewport must win over the size saved in the profile"
  );
}

/// Read the WebSocket endpoint out of a Chrome we launched ourselves,
/// the way an external browser manager's discover command would.
fn devtools_ws_url(stderr: std::process::ChildStderr) -> String {
  use std::io::BufRead;
  for line in std::io::BufReader::new(stderr).lines() {
    let line = line.expect("chrome stderr");
    if let Some(url) = line.split("DevTools listening on ").nth(1) {
      return url.to_string();
    }
  }
  panic!("chrome never announced a DevTools endpoint");
}

/// Pages adopted from a browser someone else started are emulated with
/// the configured viewport, not with that browser's window size.
///
/// This is the shape an external manager (box-dev-gate and the like)
/// produces: it launches one browser per environment at whatever size it
/// was asked for, ferridriver discovers that browser instead of starting
/// a second one, and every session works through the tab already open in
/// it. Adopting that tab without emulating anything hands the manager's
/// window size to every later call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_page_gets_the_configured_viewport() {
  let mut child = std::process::Command::new(ferridriver::state::detect_chromium())
    .args([
      "--headless=new",
      "--remote-debugging-port=0",
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "--no-sandbox",
      "--temp-profile",
      // The manager's size, unlike the default in both dimensions.
      "--window-size=1001,777",
      "about:blank",
    ])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("launch chrome");
  let ws_url = devtools_ws_url(child.stderr.take().expect("piped stderr"));

  let mut state = BrowserState::with_plan(
    ConnectMode::Launch,
    LaunchPlan {
      backend: BackendKind::CdpRaw,
      kind: BrowserKind::Chromium,
      headless: true,
      ..Default::default()
    },
  );
  state.set_instance_resolver_fn(std::sync::Arc::new(move |_| {
    Some(ConnectMode::ConnectUrl(ws_url.clone()))
  }));

  state.ensure_instance("default").await.expect("adopt browser");
  let size = state
    .active_page("default")
    .expect("an adopted page")
    .evaluate("[window.innerWidth, window.innerHeight]")
    .await
    .expect("evaluate")
    .expect("a size");
  state.shutdown().await;
  let _ = child.kill();
  let _ = child.wait();

  assert_eq!(
    size,
    serde_json::json!([1280, 720]),
    "an adopted page must be emulated with the configured viewport"
  );
}
