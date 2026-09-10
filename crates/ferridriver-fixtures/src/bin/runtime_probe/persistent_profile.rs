use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use ferridriver::backend::BackendKind;
use ferridriver::backend::cdp::ws::WsTransport;
use ferridriver::backend::process::ChildGroup;
use ferridriver::options::{BrowserKind, InstanceOverrides, LaunchPlan, ViewportConfig};
use ferridriver::state::{BrowserState, ConnectMode};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
  Chromium,
  Firefox,
  Webkit,
  Temporary,
  SavedSize,
  Adopted,
  Maximized,
}

fn profile_state(profile: &Path) -> Value {
  json!({
    "isDirectory": profile.is_dir(),
    "hasEntries": std::fs::read_dir(profile).is_ok_and(|mut entries| entries.next().is_some()),
  })
}

async fn external_browser(profile: &Path) -> Result<(ChildGroup, String)> {
  tokio::fs::create_dir(profile).await?;
  let (transport, child) = WsTransport::spawn(
    &ferridriver::state::detect_chromium(),
    profile,
    &ferridriver::state::chrome_flags(true, &["--window-size=1001,777".into()]),
    false,
    &rustc_hash::FxHashMap::default(),
  )
  .await?;
  let child = ChildGroup::recorded(child, Some(profile), false);
  drop(transport);
  let port_file = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await?;
  let mut lines = port_file.lines();
  let port = lines.next().context("DevTools port missing")?;
  let path = lines.next().context("DevTools browser path missing")?;
  Ok((child, format!("ws://127.0.0.1:{port}{path}")))
}

async fn observe(state: &mut BrowserState, scenario: Scenario, profile: &Path) -> Result<Value> {
  Box::pin(state.ensure_instance("default")).await?;
  if matches!(scenario, Scenario::Adopted) {
    let size = state
      .active_page("default")?
      .evaluate("[window.innerWidth, window.innerHeight]")
      .await?;
    return Ok(json!({ "size": size }));
  }
  let page = Box::pin(state.open_page("default", "about:blank")).await?;
  let mut maximized = Value::Null;
  if matches!(scenario, Scenario::Maximized) {
    let session = page.new_cdp_session().await?;
    let window_id = session
      .send("Browser.getWindowForTarget", json!({}))
      .await?
      .get("windowId")
      .and_then(Value::as_i64)
      .context("window ID missing")?;
    session
      .send(
        "Browser.setWindowBounds",
        json!({
          "windowId": window_id, "bounds": { "windowState": "maximized" },
        }),
      )
      .await?;
    maximized = session
      .send("Browser.getWindowBounds", json!({ "windowId": window_id }))
      .await?;
    page
      .emulate_viewport(&ViewportConfig {
        width: 900,
        height: 600,
        ..Default::default()
      })
      .await?;
  }
  let size = page.evaluate("[window.innerWidth, window.innerHeight]").await?;
  Ok(json!({ "size": size, "before": profile_state(profile), "windowBeforeResize": maximized }))
}

pub async fn run(root: &Path, scenario: Scenario) -> Result<Value> {
  let profile = root.join("persistent-profile");
  let (backend, kind) = match scenario {
    Scenario::Firefox | Scenario::Temporary => (BackendKind::Bidi, BrowserKind::Firefox),
    Scenario::Webkit => (BackendKind::WebKit, BrowserKind::WebKit),
    Scenario::Adopted => (BackendKind::CdpRaw, BrowserKind::Chromium),
    _ => (BackendKind::CdpPipe, BrowserKind::Chromium),
  };
  let mut state = BrowserState::with_plan(
    ConnectMode::Launch,
    LaunchPlan {
      backend,
      kind,
      headless: true,
      ..Default::default()
    },
  );
  if matches!(
    scenario,
    Scenario::Chromium | Scenario::Firefox | Scenario::Webkit | Scenario::SavedSize
  ) {
    let dir = profile.clone();
    state.set_instance_overrides_fn(Arc::new(move |_| {
      Ok(InstanceOverrides {
        user_data_dir: Some(dir.display().to_string()),
        ..Default::default()
      })
    }));
  }
  let mut external = if matches!(scenario, Scenario::Adopted) {
    let (child, endpoint) = external_browser(&profile).await?;
    state.set_instance_resolver_fn(Arc::new(move |_| Some(ConnectMode::ConnectUrl(endpoint.clone()))));
    Some(child)
  } else {
    None
  };
  let result = observe(&mut state, scenario, &profile).await;
  state.shutdown().await;
  if let Some(child) = &mut external {
    child.shutdown().await;
  }
  let mut result = result?;
  result["after"] = profile_state(&profile);
  Ok(result)
}
