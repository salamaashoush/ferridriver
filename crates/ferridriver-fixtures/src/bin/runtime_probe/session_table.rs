use std::time::Duration;

use anyhow::Result;
use ferridriver::options::LaunchOptions;
use ferridriver_script::{ExtensionHost, RunContext, RunOptions, ScriptEngineConfig, SessionTable};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
  max_vms: usize,
  ttl_ms: Option<u64>,
  #[serde(default)]
  browser: bool,
  actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", rename_all_fields = "camelCase")]
enum Action {
  Run {
    name: String,
    source: String,
    epoch: Option<u64>,
    timeout_ms: Option<u64>,
    #[serde(default)]
    page: bool,
  },
  LiveVms,
  Idle {
    ms: u64,
  },
}

pub async fn run(context: &RunContext, request: Request) -> Result<Value> {
  let browser = if request.browser {
    Some(
      ferridriver::chromium()
        .launch(LaunchOptions {
          headless: Some(true),
          ..Default::default()
        })
        .await?,
    )
  } else {
    None
  };
  let table = SessionTable::new(request.max_vms, request.ttl_ms.map(Duration::from_millis));
  let result: Result<Value> = async {
    let page = match &browser {
      Some(browser) => Some(browser.page().await?),
      None => None,
    };
    let mut results = Vec::with_capacity(request.actions.len());
    for action in request.actions {
      results.push(match action {
        Action::Run {
          name,
          source,
          epoch,
          timeout_ms,
          page: use_page,
        } => {
          let slot = table.acquire(&name);
          let mut session = slot.lock().await;
          let mut context = context.clone();
          context.host = ExtensionHost::Mcp;
          context.vars = session.vars();
          context.page = use_page.then(|| page.clone()).flatten();
          let result = session
            .run(
              ScriptEngineConfig::default(),
              &source,
              &[],
              RunOptions {
                timeout: timeout_ms.map(Duration::from_millis),
                ..Default::default()
              },
              context,
              epoch,
            )
            .await;
          serde_json::to_value(result)?
        },
        Action::LiveVms => json!(table.live_vm_count()),
        Action::Idle { ms } => {
          tokio::time::sleep(Duration::from_millis(ms)).await;
          Value::Null
        },
      });
    }
    Ok(json!(results))
  }
  .await;
  table.clear();
  let closed = match browser {
    Some(browser) => browser.close().await,
    None => Ok(()),
  };
  let value = result?;
  closed?;
  Ok(value)
}
