use super::{EffectiveContextConfig, after_hooks_step, artifact_dir_name, build_context_options, config_viewport};
use crate::config::{ContextConfig, ViewportConfig, expand_device_keys};

#[test]
fn an_output_dir_name_is_one_path_component_windows_accepts() {
  assert_eq!(
    artifact_dir_name("tests/e2e/tracing.test.ts", "tracing har tracing_start_har", ""),
    "tests-e2e-tracing-tracing-har-tracing_start_har"
  );
  let hostile = artifact_dir_name("C:\\specs\\a.spec.mjs", "a > b: \"c\" <d>|e? *f ../../g", "cdp-pipe");
  assert!(
    !hostile.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*', ' ']) && !hostile.contains(".."),
    "{hostile}"
  );
  let long = artifact_dir_name("a.test.ts", &"title ".repeat(30), "");
  assert_eq!(long.chars().count(), 60, "{long}");
}

#[tokio::test]
async fn a_wedged_teardown_step_names_itself_and_skips_the_rest() {
  let (_limit_tx, limit) = tokio::sync::watch::channel(std::time::Duration::from_millis(50));
  let budget = ferridriver::pause::BudgetStart::now();
  let mut stuck = None;
  assert_eq!(
    after_hooks_step(budget, &limit, &mut stuck, "capturing the failure screenshot", async {
      1
    })
    .await,
    Some(1)
  );
  assert_eq!(
    after_hooks_step(
      budget,
      &limit,
      &mut stuck,
      "saving the trace",
      std::future::pending::<()>()
    )
    .await,
    None
  );
  assert_eq!(stuck, Some("saving the trace"));
  let polled = std::sync::atomic::AtomicBool::new(false);
  let close = async { polled.store(true, std::sync::atomic::Ordering::SeqCst) };
  assert_eq!(
    after_hooks_step(budget, &limit, &mut stuck, "closing the browser context", close).await,
    None
  );
  assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
  assert_eq!(stuck, Some("saving the trace"));
}

/// A `use` block as a config layer leaves it: the device expanded,
/// then deserialized into the typed bag.
fn used(block: serde_json::Value) -> ContextConfig {
  let serde_json::Value::Object(mut map) = block else {
    panic!("a use block is an object");
  };
  expand_device_keys(&mut map);
  serde_json::from_value(serde_json::Value::Object(map)).expect("context config")
}

fn effective(context: ContextConfig, fallback: Option<ViewportConfig>) -> EffectiveContextConfig {
  let mut browser = crate::config::BrowserConfig::default();
  browser.viewport = fallback.map(ferridriver_config::browser::ViewportOverride::Size);
  browser.use_options = context.clone();
  EffectiveContextConfig {
    context,
    default_viewport: config_viewport(&browser),
    viewport_override: None,
    request_base_url: None,
  }
}

#[test]
fn throttling_in_the_use_block_reaches_the_context_options() {
  let context = used(serde_json::json!({ "networkConditions": "slow-4g", "cpuThrottling": 4 }));
  let opts = build_context_options(
    &effective(context, None),
    std::path::Path::new("."),
    ferridriver::backend::BackendKind::CdpPipe,
  );
  assert_eq!(
    opts.network_conditions,
    ferridriver::options::NetworkConditions::preset("slow-4g")
  );
  assert_eq!(opts.cpu_throttling_rate, Some(4.0));
}

#[test]
fn an_unknown_network_preset_fails_the_config_rather_than_running_unthrottled() {
  let parsed: Result<ContextConfig, _> = serde_json::from_value(serde_json::json!({ "networkConditions": "dial-up" }));
  let message = parsed.err().map(|e| e.to_string()).unwrap_or_default();
  assert!(message.contains("unknown network preset"), "{message}");
}

#[test]
fn a_devices_use_keys_reach_the_context_options() {
  let context = used(serde_json::json!({ "device": "iPhone 15" }));
  let effective = effective(context, Some(ViewportConfig::default()));
  let opts = build_context_options(
    &effective,
    std::path::Path::new("."),
    ferridriver::backend::BackendKind::CdpPipe,
  );

  assert!(matches!(
    opts.viewport,
    ferridriver::options::ViewportOption::Size {
      width: 393,
      height: 659
    }
  ));
  let screen = opts.screen.expect("the descriptor's screen reaches the context");
  assert_eq!((screen.width, screen.height), (393, 852));
  assert_eq!(opts.device_scale_factor, Some(3.0));
  assert_eq!(opts.is_mobile, Some(true));
  assert_eq!(opts.has_touch, Some(true));
  assert!(opts.user_agent.expect("userAgent").contains("iPhone"));
}

#[test]
fn a_use_viewport_wins_over_the_browsers_own() {
  let context = used(serde_json::json!({ "viewport": { "width": 411, "height": 823 } }));
  let effective = effective(
    context,
    Some(ViewportConfig {
      width: 1280,
      height: 720,
    }),
  );
  assert!(matches!(
    build_context_options(
      &effective,
      std::path::Path::new("."),
      ferridriver::backend::BackendKind::CdpPipe,
    )
    .viewport,
    ferridriver::options::ViewportOption::Size {
      width: 411,
      height: 823
    }
  ));
}

#[test]
fn a_null_use_viewport_clears_the_browsers_own() {
  let context = used(serde_json::json!({ "viewport": null }));
  let effective = effective(
    context,
    Some(ViewportConfig {
      width: 1280,
      height: 720,
    }),
  );
  assert!(
    matches!(
      effective.default_viewport,
      Some(ferridriver_config::browser::ViewportOverride::Disabled)
    ),
    "`viewport: null` is not the same as leaving it out"
  );
  assert_eq!(
    build_context_options(
      &effective,
      std::path::Path::new("."),
      ferridriver::backend::BackendKind::CdpPipe
    )
    .viewport,
    ferridriver::options::ViewportOption::Null
  );
}

#[test]
fn default_and_disabled_viewports_do_not_share_pooled_contexts() {
  let default = effective(ContextConfig::default(), None);
  let disabled = effective(used(serde_json::json!({"viewport":null})), None);
  let backend = ferridriver::backend::BackendKind::CdpPipe;
  assert_ne!(default.pool_key(backend), disabled.pool_key(backend));
  assert_eq!(
    build_context_options(&default, std::path::Path::new("."), backend).viewport,
    ferridriver::options::ViewportOption::Default
  );
}
