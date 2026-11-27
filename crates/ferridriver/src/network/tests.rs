//! Unit tests for `Request`, `Response`, `RedirectChain`, `WebSocket`, and header helpers.

use super::*;

fn sample_request(id: &str, url: &str, redirected_from: Option<Request>) -> Request {
  Request::new(RequestInit {
    id: id.into(),
    url: url.into(),
    method: "GET".into(),
    resource_type: "Document".into(),
    is_navigation_request: true,
    post_data: None,
    headers: Headers::default(),
    frame_id: None,
    page_guid: None,
    redirected_from,
    timing: None,
    raw_headers_fn: None,
  })
}

fn sample_response(request: Request, status: i64, headers: Headers) -> Response {
  Response::new(ResponseInit {
    url: request.url().into(),
    request,
    status,
    status_text: String::new(),
    from_service_worker: false,
    http_version: None,
    headers,
    remote_addr: None,
    security_details: None,
    body_fn: None,
    raw_headers_fn: None,
  })
}

#[test]
fn only_text_and_binary_frames_with_data_are_reported() {
  let frame = |opcode: u64, data: &str| serde_json::json!({ "response": { "opcode": opcode, "payloadData": data } });
  assert!(matches!(websocket_frame_payload(&frame(1, "hi")), Some(WebSocketPayload::Text(t)) if t == "hi"));
  assert!(matches!(
    websocket_frame_payload(&frame(2, "AQL/")),
    Some(WebSocketPayload::Binary(b)) if b == [1, 2, 255]
  ));
  assert!(websocket_frame_payload(&frame(1, "")).is_none(), "no payload");
  assert!(
    websocket_frame_payload(&frame(8, "\u{3}\u{e8}")).is_none(),
    "the close handshake"
  );
  assert!(websocket_frame_payload(&frame(9, "ping")).is_none());
}

#[tokio::test]
async fn navigation_waits_for_redirect_hops_queued_after_the_lifecycle_result() {
  let slot = NavRequestSlot::new();
  let loc = || [("location".into(), "/next".into())].into_iter().collect();

  let first = sample_request("reused-bidi-id", "http://example.test/", None);
  first.set_response(&sample_response(first.clone(), 302, loc())).await;
  slot.set(first.clone());
  let mut waiting = Box::pin(slot.final_response(std::time::Duration::from_secs(1)));
  assert!(futures::poll!(&mut waiting).is_pending());

  let second = sample_request("reused-bidi-id", "http://example.test/", Some(first));
  second.set_response(&sample_response(second.clone(), 307, loc())).await;
  slot.set(second.clone());
  assert!(futures::poll!(&mut waiting).is_pending());

  let final_request = sample_request("reused-bidi-id", "http://example.test/", Some(second));
  final_request
    .set_response(&sample_response(final_request.clone(), 200, loc()))
    .await;
  slot.set(final_request);
  assert_eq!(waiting.await.expect("final response").status(), 200);
}

#[test]
fn a_navigation_takes_its_own_loaders_request() {
  let slot = NavRequestSlot::new();
  slot.set_for_loader(
    sample_request("http://example.test/page", "http://example.test/page", None),
    "page".into(),
  );
  // The cancelled navigation's request, reported after the new one.
  slot.set_for_loader(
    sample_request("http://example.test/cancelled", "http://example.test/cancelled", None),
    "cancelled".into(),
  );
  assert_eq!(slot.get_for(Some("page")).unwrap().url(), "http://example.test/page");
  assert_eq!(slot.get().unwrap().url(), "http://example.test/cancelled");

  // A child frame's document after the page's, then the page commits.
  slot.set_for_loader(
    sample_request("http://example.test/frame", "http://example.test/frame", None),
    "frame".into(),
  );
  slot.commit_loader("page".into());
  assert_eq!(slot.get().unwrap().url(), "http://example.test/page");
  assert!(slot.get_for(Some("missing")).is_none());

  // A request with no loader stands for any.
  slot.clear();
  slot.set(sample_request(
    "http://example.test/any",
    "http://example.test/any",
    None,
  ));
  assert_eq!(slot.get_for(Some("page")).unwrap().url(), "http://example.test/any");
}

#[tokio::test]
async fn redirect_chain_links_in_both_directions() {
  let original = sample_request("1", "http://x/a", None);
  let next = sample_request("2", "http://x/b", Some(original.clone()));

  assert_eq!(next.redirected_from().unwrap().url(), "http://x/a");
  assert_eq!(original.redirected_to().unwrap().url(), "http://x/b");
}

#[tokio::test]
async fn request_response_and_redirect_chain_drop_cleanly_without_arc_cycles() {
  let hop0 = sample_request("1", "http://x/0", None);
  let res0 = sample_response(hop0.clone(), 302, Headers::default());
  hop0.set_response(&res0).await;

  let hop1 = sample_request("2", "http://x/1", Some(hop0.clone()));
  let res1 = sample_response(hop1.clone(), 307, Headers::default());
  hop1.set_response(&res1).await;

  let hop2 = sample_request("3", "http://x/2", Some(hop1.clone()));
  let res2 = sample_response(hop2.clone(), 200, Headers::default());
  hop2.set_response(&res2).await;

  let weak_requests = [
    Arc::downgrade(&hop0.inner),
    Arc::downgrade(&hop1.inner),
    Arc::downgrade(&hop2.inner),
  ];
  let weak_responses = [
    Arc::downgrade(&res0.inner),
    Arc::downgrade(&res1.inner),
    Arc::downgrade(&res2.inner),
  ];

  // Drop every handle except the final Response: the whole chain and every
  // response remain reachable through `res2.request()`.
  drop((hop0, hop1, hop2, res0, res1));
  assert_eq!(res2.request().url(), "http://x/2");
  let mid = res2.request().redirected_from().unwrap();
  assert_eq!(mid.url(), "http://x/1");
  assert_eq!(mid.existing_response().await.unwrap().status(), 307);
  assert_eq!(mid.redirected_from().unwrap().url(), "http://x/0");
  assert_eq!(mid.redirected_to().unwrap().url(), "http://x/2");
  drop(mid);

  // Dropping the last external handle frees every RequestState and ResponseState.
  drop(res2);
  for (hop, weak) in weak_requests.iter().enumerate() {
    assert!(weak.upgrade().is_none(), "hop{hop} RequestState leaked");
  }
  for (hop, weak) in weak_responses.iter().enumerate() {
    assert!(weak.upgrade().is_none(), "hop{hop} ResponseState leaked");
  }
}

#[test]
fn headers_array_to_map_joins_set_cookie_with_newlines() {
  let arr = vec![
    HeaderEntry {
      name: "Set-Cookie".into(),
      value: "a=1".into(),
    },
    HeaderEntry {
      name: "Set-Cookie".into(),
      value: "b=2".into(),
    },
    HeaderEntry {
      name: "Content-Type".into(),
      value: "text/plain".into(),
    },
  ];
  let map = headers_array_to_map(&arr);
  assert_eq!(map.get("set-cookie").unwrap(), "a=1\nb=2");
  assert_eq!(map.get("content-type").unwrap(), "text/plain");
}

#[test]
fn ok_status_matches_playwright_semantics() {
  let assert_status = |s: i64, expected: bool| {
    let r = sample_response(sample_request("x", "http://x/", None), s, Headers::default());
    assert_eq!(r.ok(), expected, "status {s}");
  };
  assert_status(0, true);
  assert_status(200, true);
  assert_status(204, true);
  assert_status(299, true);
  assert_status(300, false);
  assert_status(404, false);
  assert_status(500, false);
}
