use std::sync::Arc;

use anyhow::{Context, Result};
use ferridriver::options::{BrowserContextOptions, LaunchOptions};
use ferridriver::protocol::SerializedArgument;
use ferridriver::{Browser, Page, chromium};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Action {
  Registries,
  NewContext { user_agent: Option<String> },
  NewPage,
  SetContent { html: String },
  PageState,
  SelfClose,
  Evaluate { expression: String },
  CloseContext,
}

struct Lifecycle {
  browser: Browser,
  context: Option<ferridriver::context::ContextRef>,
  page: Option<Arc<Page>>,
}

impl Lifecycle {
  fn context(&self) -> Result<&ferridriver::context::ContextRef> {
    self.context.as_ref().context("no context")
  }

  fn page(&self) -> Result<&Arc<Page>> {
    self.page.as_ref().context("no page")
  }

  async fn run(&mut self, action: Action) -> Result<Value> {
    match action {
      Action::Registries => registries(&self.browser).await,
      Action::NewContext { user_agent } => {
        self.context = Some(
          self
            .browser
            .new_context()
            .options(BrowserContextOptions {
              user_agent,
              ..Default::default()
            })
            .await?,
        );
        self.page = None;
        Ok(Value::Null)
      },
      Action::NewPage => {
        self.page = Some(self.context()?.new_page().await?);
        Ok(Value::Null)
      },
      Action::SetContent { html } => {
        self.page()?.set_content(&html).await?;
        Ok(Value::Null)
      },
      Action::PageState => {
        let closed = self.page()?.is_closed();
        let state = self.browser.state().read().await;
        let active_closed = state
          .context(self.context()?.name())?
          .active_page()
          .map(ferridriver::backend::AnyPage::is_closed);
        Ok(json!({ "closed": closed, "activeClosed": active_closed }))
      },
      Action::SelfClose => {
        let page = self.page()?;
        let closed = page.inner().events().wait_for_event("close", 10_000);
        let evaluated = page
          .evaluate("window.close()", SerializedArgument::default(), None)
          .await;
        closed.await?;
        Ok(json!({ "evaluationError": evaluated.err().map(|error| error.to_string()) }))
      },
      Action::Evaluate { expression } => Ok(json!(
        self
          .page()?
          .evaluate(&expression, SerializedArgument::default(), None)
          .await?
          .to_json_like()
      )),
      Action::CloseContext => {
        self.context()?.close().await?;
        Ok(Value::Null)
      },
    }
  }
}

async fn registries(browser: &Browser) -> Result<Value> {
  let state = browser.state().read().await;
  let mut sizes = serde_json::Map::new();
  macro_rules! sync_size {
    ($($field:ident),* $(,)?) => {$(
      sizes.insert(stringify!($field).into(), json!(state.$field.lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?.len()));
    )*};
  }
  sync_size!(
    context_options,
    context_events,
    context_closed,
    record_video,
    har_recorders,
    context_har_updates,
    clock_installed,
    storage_state_hydrated
  );
  sizes.insert(
    "context_bindings".into(),
    json!(state.context_bindings.read().await.len()),
  );
  sizes.insert(
    "context_ws_routes".into(),
    json!(state.context_ws_routes.read().await.len()),
  );
  sizes.insert("context_routes".into(), json!(state.context_routes.read().await.len()));
  sizes.insert(
    "context_init_scripts".into(),
    json!(state.context_init_scripts.read().await.len()),
  );
  Ok(Value::Object(sizes))
}

pub async fn run(actions: Vec<Action>) -> Result<Value> {
  let browser = chromium()
    .launch(LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await?;
  let mut lifecycle = Lifecycle {
    browser,
    context: None,
    page: None,
  };
  let result: Result<Value> = async {
    let mut results = Vec::new();
    for action in actions {
      results.push(lifecycle.run(action).await?);
    }
    Ok(json!(results))
  }
  .await;
  let closed = lifecycle.browser.close().await;
  let observations = result?;
  closed?;
  Ok(observations)
}
