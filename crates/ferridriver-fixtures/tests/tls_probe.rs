#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Proves the self-signed TLS fixture is rejected by default and
//! accepted with `danger_accept_invalid_certs`, so a failure in the
//! engine's per-request `ignoreHTTPSErrors` cannot be blamed on the
//! fixture.

use ferridriver_fixtures::{FixtureServer, FixtureServerOptions};

#[tokio::test]
async fn self_signed_fixture_is_rejected_by_default_and_accepted_when_ignored() {
  let server = FixtureServer::start(FixtureServerOptions::default()).await.unwrap();
  let url = format!("{}/secure", server.tls_url());

  let strict = reqwest::Client::builder().build().unwrap();
  let strict_err = strict.get(&url).send().await;
  assert!(strict_err.is_err(), "a self-signed cert must be rejected by default");

  let lax = reqwest::Client::builder()
    .danger_accept_invalid_certs(true)
    .build()
    .expect("builder with danger_accept_invalid_certs must build");
  let resp = lax.get(&url).send().await.expect("lax client reaches the fixture");
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.unwrap(), "secured!!");

  server.stop().await;
}
