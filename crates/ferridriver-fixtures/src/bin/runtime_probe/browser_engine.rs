use anyhow::Result;
use ferridriver::options::LaunchOptions;
use ferridriver_script::{RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct Script {
  source: String,
  #[serde(default)]
  args: Vec<Value>,
}

pub async fn run(context: &RunContext, scripts: Vec<Script>) -> Result<Value> {
  let browser = ferridriver::chromium()
    .launch(LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await?;
  let result: Result<Value> = async {
    let mut context = context.clone();
    context.page = Some(browser.page().await?);
    let engine = ScriptEngine::new(ScriptEngineConfig::default());
    let mut results = Vec::with_capacity(scripts.len());
    for script in scripts {
      results.push(
        engine
          .run(&script.source, &script.args, RunOptions::default(), context.clone())
          .await,
      );
    }
    Ok(serde_json::to_value(results)?)
  }
  .await;
  let closed = browser.close().await;
  let value = result?;
  closed?;
  Ok(value)
}
