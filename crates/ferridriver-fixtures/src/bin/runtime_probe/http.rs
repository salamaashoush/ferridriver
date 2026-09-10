use std::sync::Arc;

use anyhow::Result;
use ferridriver::http_client::{HttpClient, HttpClientOptions, NetGuard, NetPolicy, RequestOptions};
use ferrijs_permissions::Permissions;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Request {
  url: String,
  guard: Option<Guard>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Guard {
  allowlist: Option<Vec<String>>,
  block_metadata: bool,
  block_private: bool,
}

impl Guard {
  fn into_options(self) -> Result<RequestOptions> {
    let policy = self
      .allowlist
      .map(|hosts| Permissions::none().allow_net(hosts).map_err(anyhow::Error::msg))
      .transpose()?
      .map(|permissions| Arc::new(permissions) as Arc<dyn NetPolicy>);
    Ok(RequestOptions {
      net_guard: Some(NetGuard {
        policy,
        block_metadata: self.block_metadata,
        block_private: self.block_private,
      }),
      ..Default::default()
    })
  }
}

pub async fn run(request: Request) -> Result<Value> {
  let options = request.guard.map(Guard::into_options).transpose()?;
  let client = HttpClient::new(HttpClientOptions::default());
  let response = client.get(&request.url, options).await?;
  Ok(json!({ "status": response.status(), "body": response.text()? }))
}
