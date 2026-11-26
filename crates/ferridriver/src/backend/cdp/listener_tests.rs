//! A page listener hears everything that arrives once it has been
//! started, including what arrives before its task is first polled.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::CdpPage;
use super::transport::{CdpDispatcher, CdpTransport};

#[tokio::test]
async fn a_dialog_that_opens_before_the_listener_is_first_polled_is_delivered() {
  let dispatcher = Arc::new(CdpDispatcher::new());
  let transport = Arc::new(CdpTransport::from_test_dispatcher(Arc::clone(&dispatcher)));
  let manager = crate::dialog::DialogManager::new();
  let opened = Arc::new(AtomicUsize::new(0));
  let counted = Arc::clone(&opened);
  manager.add_handler(Arc::new(move |dialog| {
    assert_eq!(dialog.message(), "boolean?");
    counted.fetch_add(1, Ordering::SeqCst);
    true
  }));

  let listener = CdpPage::spawn_dialog_listener(
    Arc::clone(&transport),
    Some(Arc::from("session")),
    Arc::default(),
    crate::events::EventEmitter::new(),
    manager,
    crate::backend::PageBackref::new(),
  );
  // No await since the spawn: on this single-threaded runtime the
  // listener's task has not run yet, which is the moment a loaded
  // runtime leaves open between starting a page and its first dialog.
  dispatcher.dispatch_message(
    br#"{"sessionId":"session","method":"Page.javascriptDialogOpening","params":{"type":"confirm","message":"boolean?","defaultPrompt":""}}"#,
  );

  tokio::time::timeout(std::time::Duration::from_secs(5), async {
    while opened.load(Ordering::SeqCst) == 0 {
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("the dialog reached its handler");
  assert_eq!(opened.load(Ordering::SeqCst), 1);
  listener.abort();
}
