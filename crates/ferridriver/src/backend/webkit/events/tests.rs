//! Unit tests for `WebKit` event dispatch and cookie/frame helpers.

use super::dispatch::{apply_frame_event, handle_frame_navigated, lock_cache};
use super::interception::fulfilled_mime_type;
use super::network::parse_remote_address;
use super::*;

#[cfg(test)]
mod frame_event_tests {
  use super::*;

  #[tokio::test(flavor = "current_thread")]
  async fn navigation_notification_preserves_children_restored_after_the_wire_event() {
    let cache = Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::new("initial")));
    let observed = Arc::new(std::sync::Mutex::new(crate::observed::ObservedBuffers::default()));
    let emitter = crate::events::EventEmitter::new();
    assert!(emitter.set_state_observer(Arc::new(crate::page::page_state_observer(
      crate::backend::BackendKind::WebKit,
      Arc::clone(&cache),
      observed,
      None,
    ))));
    let mut events = emitter.subscribe();
    let root = crate::backend::FrameInfo {
      frame_id: "main".into(),
      parent_frame_id: None,
      name: String::new(),
      url: "https://example.com/restored".into(),
    };
    let navigated = crate::backend::webkit::protocol::Envelope {
      id: None,
      method: Some("Page.frameNavigated".into()),
      params: serde_json::json!({"frame": {"id": root.frame_id, "url": root.url}}),
      result: None,
      error: None,
      page_proxy_id: None,
    };
    apply_frame_event(&mut lock_cache(&cache), &navigated);
    handle_frame_navigated(&navigated.params, &emitter);
    cache.lock().unwrap().seed(vec![
      root,
      crate::backend::FrameInfo {
        frame_id: "child".into(),
        parent_frame_id: Some("main".into()),
        name: "child".into(),
        url: "about:srcdoc".into(),
      },
    ]);
    assert!(matches!(
      events.recv().await,
      Some(crate::events::PageEvent::FrameNavigated(_))
    ));
    let current = cache.lock().unwrap();
    assert_eq!(current.live_ids().count(), 2);
    assert!(!current.record("child").unwrap().detached);
  }
}

#[cfg(test)]
mod event_tests {
  use super::*;
  fn fulfilled(content_type: Option<&str>, headers: &[(&str, &str)], body: &str) -> String {
    fulfilled_mime_type(&crate::route::FulfillResponse {
      status: 200,
      headers: headers
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect(),
      body: body.as_bytes().to_vec(),
      content_type: content_type.map(str::to_string),
    })
  }

  #[test]
  fn a_fulfilled_body_is_parsed_as_its_content_type_s_essence_without_parameters() {
    assert_eq!(fulfilled(Some("text/html; charset=utf-8"), &[], ""), "text/html");
    assert_eq!(fulfilled(Some(" text/css ;charset=utf-8"), &[], ""), "text/css");
    assert_eq!(fulfilled(Some("font/woff2"), &[], ""), "font/woff2");
    assert_eq!(
      fulfilled(None, &[("Content-Type", "text/html; charset=utf-8")], ""),
      "text/html"
    );
    assert_eq!(
      fulfilled(
        None,
        &[
          ("content-type", "text/plain"),
          ("CONTENT-TYPE", "text/css; charset=utf-8")
        ],
        ""
      ),
      "text/css"
    );
    assert_eq!(
      fulfilled(Some("image/png"), &[("content-type", "text/html")], ""),
      "image/png"
    );
  }

  #[test]
  fn a_fulfilled_body_with_no_content_type_or_an_empty_one_is_sniffed() {
    assert_eq!(fulfilled(None, &[], "<!doctype html><p>a</p>"), "text/html");
    assert_eq!(fulfilled(None, &[], "plain words"), "text/plain");
    assert_eq!(fulfilled(Some(""), &[], "<!doctype html><p>a</p>"), "text/html");
    assert_eq!(fulfilled(Some(" ; charset=utf-8"), &[], "plain words"), "text/plain");
    assert_eq!(
      fulfilled(None, &[("content-type", "")], "<!doctype html><p>a</p>"),
      "text/html"
    );
  }

  #[test]
  fn a_remote_address_splits_on_its_last_colon() {
    let v4 = parse_remote_address("127.0.0.1:8443").unwrap();
    assert_eq!((v4.ip_address.as_str(), v4.port), ("127.0.0.1", 8443));
    let v6 = parse_remote_address("::1:443").unwrap();
    assert_eq!((v6.ip_address.as_str(), v6.port), ("[::1]", 443));
    assert!(parse_remote_address("no-port").is_none());
  }

  #[test]
  fn only_the_live_target_crashing_is_a_crash() {
    let crashed = serde_json::json!({ "targetId": "page-2", "crashed": true });
    assert!(super::live_target_crashed(&crashed, "page-2"));
    assert!(!super::live_target_crashed(&crashed, "page-3"), "a replaced target");
    let swapped = serde_json::json!({ "targetId": "page-2" });
    assert!(
      !super::live_target_crashed(&swapped, "page-2"),
      "a process swap destroys without crashing"
    );
  }
}
