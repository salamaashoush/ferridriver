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
