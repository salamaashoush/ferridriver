use std::time::Duration;

use anyhow::Result;
use ferridriver_script::sidecar::{Sidecar, SidecarError, SidecarSpec};
use futures::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Call {
  method: String,
  params: Option<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
  Send { call: Call },
  Concurrent { calls: Vec<Call> },
  Batch { calls: Vec<Call> },
  Event,
  Closed,
  WaitClosed,
  ExitWithWaiters,
  Close,
}

fn outcome(result: Result<Value, SidecarError>) -> Value {
  match result {
    Ok(value) => json!({ "value": value }),
    Err(error) => json!({ "error": error.to_string() }),
  }
}

pub async fn run(binary: String, actions: Vec<Action>) -> Result<Value> {
  let sidecar = Sidecar::connect(&SidecarSpec {
    name: "echo".into(),
    command: vec![binary],
    env: Vec::new(),
    cwd: None,
  })
  .await?;
  let mut events = sidecar.subscribe();
  let result: Result<Value> = async {
    let mut results = Vec::with_capacity(actions.len());
    for action in actions {
      results.push(match action {
        Action::Send { call } => outcome(sidecar.send(&call.method, call.params, 5000).await),
        Action::Concurrent { calls } => json!(
          join_all(
            calls
              .iter()
              .map(|call| sidecar.send(&call.method, call.params.clone(), 5000))
          )
          .await
          .into_iter()
          .map(outcome)
          .collect::<Vec<_>>()
        ),
        Action::Batch { calls } => json!(
          sidecar
            .send_many(calls.into_iter().map(|call| (call.method, call.params)).collect(), 5000)
            .await
            .into_iter()
            .map(outcome)
            .collect::<Vec<_>>()
        ),
        Action::Event => json!(tokio::time::timeout(Duration::from_secs(5), events.recv()).await??),
        Action::Closed => json!(sidecar.is_closed()),
        Action::WaitClosed => {
          tokio::time::timeout(Duration::from_secs(5), sidecar.wait_closed()).await?;
          json!(sidecar.is_closed())
        },
        Action::ExitWithWaiters => {
          let ((), (), result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
              sidecar.wait_closed(),
              sidecar.wait_closed(),
              sidecar.send("exit", None, 1000)
            )
          })
          .await?;
          json!({ "closed": sidecar.is_closed(), "exit": outcome(result) })
        },
        Action::Close => {
          sidecar.close().await?;
          Value::Null
        },
      });
    }
    Ok(json!(results))
  }
  .await;
  let closed = sidecar.close().await;
  let value = result?;
  closed?;
  Ok(value)
}
