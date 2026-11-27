//! Unit tests for `WebKitPage` lifecycle latches, user-agent platform mapping, and shims.

use super::{
  DESKTOP_ORIENTATION_SHIM, EngineLatch, LifecycleSignals, NavLifecycle, SAFARI_SURFACE_SHIM, WebKitPage,
  navigator_platform_for,
};

#[test]
fn the_previous_document_loading_late_does_not_finish_a_navigation() {
  let signals = LifecycleSignals::default();
  signals.reset();
  signals.mark(NavLifecycle::Load);
  assert!(
    !signals.committed(Some("next")),
    "the old document's load arrived first"
  );

  signals.commit_document(Some("next".into()));
  assert!(signals.committed(Some("next")));
  assert!(
    !signals.seen(NavLifecycle::Load),
    "a commit clears what the old document fired"
  );
  assert!(signals.seen(NavLifecycle::Commit));

  signals.mark(NavLifecycle::Load);
  assert!(signals.seen(NavLifecycle::Load) && signals.committed(Some("next")));
  assert!(signals.committed(None));
}

#[test]
fn an_injection_into_a_replaced_document_does_not_count() {
  let latch = EngineLatch::default();
  let first = latch.pending().expect("nothing injected yet");
  latch.reset();
  assert!(!latch.injected(first));
  let second = latch.pending().expect("the new document has no engine");
  assert_ne!(first, second);
  assert!(latch.injected(second));
  assert_eq!(latch.pending(), None);
  latch.reset();
  assert!(latch.pending().is_some_and(|third| third != second));
}

#[tokio::test]
async fn a_call_fails_once_its_document_is_replaced_and_not_before() {
  let latch = EngineLatch::default();
  latch.context_created(1);
  latch.context_created(2);
  let mut old = latch.watch(1);
  assert!(!old.is_gone());
  latch.document_replaced();
  old.gone().await;
  latch.context_created(3);
  assert!(latch.watch(2).is_gone());
  let mut current = latch.watch(3);
  assert!(!current.is_gone());
  // Another target numbers its contexts afresh; what ran on the old one is gone.
  latch.target_replaced();
  current.gone().await;
  latch.context_created(1);
  assert!(!latch.watch(1).is_gone());
}

#[test]
fn platform_follows_the_emulated_user_agent() {
  let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
  let ipad = "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
  let android =
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Mobile Safari/537.36";
  let mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

  // An iPad UA also carries "Mac OS X", so order decides the answer.
  assert_eq!(navigator_platform_for(iphone), Some("iPhone"));
  assert_eq!(navigator_platform_for(ipad), Some("iPad"));
  assert_eq!(navigator_platform_for(android), Some("Linux armv8l"));
  assert_eq!(navigator_platform_for(mac), Some("MacIntel"));
  assert_eq!(navigator_platform_for("some custom agent"), None);
}

#[test]
fn mobile_settings_invert_between_desktop_and_mobile() {
  let mobile = WebKitPage::mobile_settings(true);
  let desktop = WebKitPage::mobile_settings(false);

  for ((name, mobile_value), (_, desktop_value)) in mobile.iter().zip(desktop.iter()) {
    assert_ne!(mobile_value, desktop_value, "{name} should differ between modes");
  }
  assert!(
    mobile.contains(&("DeviceOrientationEventEnabled", true)),
    "a mobile page must fire orientation events"
  );
}

#[test]
fn the_shim_supplies_the_surface_this_build_lacks() {
  assert!(SAFARI_SURFACE_SHIM.contains("window.GestureEvent"));
  assert!(SAFARI_SURFACE_SHIM.contains("window.safari"));
  assert!(SAFARI_SURFACE_SHIM.contains("PublicKeyCredential"));
  assert!(DESKTOP_ORIENTATION_SHIM.contains("delete window.orientation"));
}
