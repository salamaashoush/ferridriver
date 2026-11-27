//! Unit tests for browser instance routing, launch commands, and discovery.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::command_spec::ResolvedExec;

use super::*;

/// Tests that need a port to be DEAD must not run while a sibling is
/// binding `:0` — the kernel readily hands out the port one test just
/// freed, and the "dead" assertion then fails for an unrelated reason.
static PORT_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn port_guard() -> std::sync::MutexGuard<'static, ()> {
  PORT_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn spec(json: &str) -> CommandSpec {
  serde_json::from_str(json).expect("spec")
}

/// A shell-form spec: `unix` runs under `sh`, `windows` under `cmd.exe`,
/// with `extra` fields (`cwd`, `timeoutMs`) beside `run`. Built as a JSON
/// value, so a path in either line needs no escaping.
fn shell(unix: &str, windows: &str, extra: serde_json::Value) -> CommandSpec {
  let mut object = extra;
  object["run"] = serde_json::Value::String(if cfg!(windows) { windows } else { unix }.to_string());
  serde_json::from_value(object).expect("spec")
}

fn view<'a>(
  instances: &'a rustc_hash::FxHashMap<String, InstanceConfig>,
  args: Option<&'a CommandSpec>,
  discover: Option<&'a CommandSpec>,
  cache: &'a CommandCache,
) -> RoutingView<'a> {
  RoutingView {
    browser: None,
    instances,
    global: None,
    default_instance: None,
    args_command: args,
    discover_command: discover,
    cache,
    cache_ttl: DEFAULT_CACHE_TTL,
    backend: BackendKind::CdpPipe,
  }
}

/// The failure an operator actually hits: a name no instance claims used to
/// reach the args command, so the error was that command's usage message
/// instead of the configured set — the one piece of information that answers it.
#[test]
fn an_unknown_instance_names_the_configured_ones_instead_of_running_the_command() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert("staging".to_string(), InstanceConfig::default());
  instances.insert("dev".to_string(), InstanceConfig::default());
  // Would exit non-zero with its own message if it were ever reached.
  let args = shell(
    "echo boom >&2; exit 2",
    "echo boom 1>&2 & exit 2",
    serde_json::json!({}),
  );
  let cache = CommandCache::default();
  let v = view(&instances, Some(&args), None, &cache);

  for err in [
    v.health("desktop").expect_err("health must refuse"),
    v.overrides_for("desktop").expect_err("overrides must refuse"),
  ] {
    assert!(err.contains("unknown instance 'desktop'"), "{err}");
    assert!(err.contains("dev, staging"), "must name the configured set: {err}");
    assert!(!err.contains("boom"), "the args command must not have run: {err}");
  }

  // A declared name reaches the command, so it fails as the COMMAND failing —
  // which is what proves the gate did not swallow the declared case too.
  let ran = v.health("staging").expect_err("the failing command still runs");
  assert!(ran.contains("its args command failed"), "{ran}");
  assert!(!ran.contains("unknown instance"), "{ran}");
}

/// A launcher that manages the profile, the binary and the proxy should be
/// able to say so — otherwise the operator hand-copies a managed path into
/// config, and the copy goes stale the moment the launcher moves it.
#[test]
fn an_args_command_supplies_the_whole_launch_shape() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert("staging".to_string(), InstanceConfig::default());
  let launch =
    r#"{"args":["--flag"],"profileDir":"/managed/staging","executablePath":"/bin/cft","env":{"FROM_CMD":"1"}}"#;
  let args = shell(
    &format!("printf '{launch}'"),
    &format!("echo {launch}"),
    serde_json::json!({}),
  );
  let cache = CommandCache::default();
  let v = view(&instances, Some(&args), None, &cache);

  let out = v.overrides_for("staging").expect("overrides");
  assert_eq!(out.args, vec!["--flag".to_string()]);
  assert_eq!(out.user_data_dir.as_deref(), Some("/managed/staging"));
  assert_eq!(out.executable_path.as_deref(), Some("/bin/cft"));
  assert_eq!(out.env.get("FROM_CMD").map(String::as_str), Some("1"));
}

/// A value the operator wrote down is never replaced by a derived one.
#[test]
fn explicit_config_outranks_the_command_result() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "staging".to_string(),
    InstanceConfig {
      user_data_dir: Some("/chosen".to_string()),
      executable_path: Some("/chosen/chrome".to_string()),
      env: [("FROM_CMD".to_string(), "config".to_string())].into_iter().collect(),
      ..InstanceConfig::default()
    },
  );
  let launch = r#"{"args":[],"profileDir":"/managed","executablePath":"/managed/chrome","env":{"FROM_CMD":"command"}}"#;
  let args = shell(
    &format!("printf '{launch}'"),
    &format!("echo {launch}"),
    serde_json::json!({}),
  );
  let cache = CommandCache::default();
  let v = view(&instances, Some(&args), None, &cache);

  let out = v.overrides_for("staging").expect("overrides");
  assert_eq!(out.user_data_dir.as_deref(), Some("/chosen"));
  assert_eq!(out.executable_path.as_deref(), Some("/chosen/chrome"));
  assert_eq!(out.env.get("FROM_CMD").map(String::as_str), Some("config"));
}

/// The schema is ours, so it must accept the shapes we document and refuse
/// the ones that carry no settings — never silently launch an unconfigured
/// browser because a command returned something unexpected.
#[test]
fn the_launch_spec_schema_accepts_what_we_document() {
  // Legacy shapes stay supported.
  let arr = parse_command_result(&serde_json::json!(["--a", "--b"])).expect("array");
  assert_eq!(arr.args, vec!["--a".to_string(), "--b".to_string()]);
  let text = parse_command_result(&serde_json::json!("--a\n--b\n")).expect("text");
  assert_eq!(text.args, vec!["--a".to_string(), "--b".to_string()]);

  // Both spellings of the profile directory.
  for key in ["userDataDir", "profileDir"] {
    let v = serde_json::json!({ key: "/managed" });
    let spec = parse_command_result(&v).expect(key);
    assert_eq!(spec.user_data_dir.as_deref(), Some("/managed"), "for {key}");
  }

  // A field we do not know must not break a launcher that adds one.
  let fwd = parse_command_result(&serde_json::json!({ "args": [], "environment": "staging", "somethingNew": 7 }))
    .expect("unknown keys are ignored");
  assert_eq!(fwd.args, [] as [String; 0]);

  // A wrong type is an error naming the key, not a dropped setting.
  let err = parse_command_result(&serde_json::json!({ "args": "not-a-list" })).expect_err("typed");
  assert!(err.contains("args"), "{err}");

  // Shapes carrying no settings at all are refused outright.
  for v in [serde_json::json!(null), serde_json::json!(true), serde_json::json!(42)] {
    assert!(parse_command_result(&v).is_err(), "must refuse {v}");
  }
}

/// A proxy the launcher reports has to reach the browser as flags, or the
/// instance silently bypasses it.
#[test]
fn a_reported_proxy_is_lowered_into_launch_flags() {
  let mut out = InstanceOverrides::default();
  let spec = InstanceLaunchSpec {
    proxy_url: Some("http://127.0.0.1:3128".to_string()),
    proxy_bypass: Some("localhost".to_string()),
    ..InstanceLaunchSpec::default()
  };
  apply_command_result(&mut out, spec, BackendKind::CdpPipe);
  assert!(
    out.args.iter().any(|a| a.contains("127.0.0.1:3128")),
    "proxy must reach the browser: {:?}",
    out.args
  );
}

/// An instance must be able to describe both halves of its own routing.
/// Without this, discovery can only address an instance by its session-key
/// label, which is the coupling `argsCommand` exists to remove.
#[test]
fn an_instance_discover_command_replaces_the_section_one() {
  let _guard = port_guard();
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
  let port = listener.local_addr().expect("addr").port();

  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "desktop".to_string(),
    InstanceConfig {
      discover_command: Some(spec(&format!(
        r#""echo ws://127.0.0.1:{port}/devtools/browser/from-instance""#
      ))),
      ..InstanceConfig::default()
    },
  );
  let section = spec(r#""echo ws://127.0.0.1:1/devtools/browser/from-section""#);
  let cache = CommandCache::default();
  let v = view(&instances, None, Some(&section), &cache);

  match v.resolve_connect("desktop") {
    Some(ConnectMode::ConnectUrl(url)) => assert!(url.contains("from-instance"), "{url}"),
    other => panic!("instance command must win outright, got {other:?}"),
  }
}

/// An instance names a browser process, and that fact does not change with
/// who is asking. Declared once at the top level, both `[mcp.browser]` and
/// `[test.browser]` must resolve it -- and a section that declares the same
/// name must win, so a host can override one entry without restating the set.
#[test]
fn a_top_level_instance_serves_every_section_and_a_section_may_override_it() {
  let mut global = BrowserSectionConfig::default();
  global.instances.insert(
    "staging".to_string(),
    InstanceConfig {
      args: vec!["--from-global".to_string()],
      ..InstanceConfig::default()
    },
  );
  global.instances.insert("prod".to_string(), InstanceConfig::default());

  // This section overrides `staging` and says nothing about `prod`.
  let mut section = rustc_hash::FxHashMap::default();
  section.insert(
    "staging".to_string(),
    InstanceConfig {
      args: vec!["--from-section".to_string()],
      ..InstanceConfig::default()
    },
  );

  let cache = CommandCache::default();
  let mut v = view(&section, None, None, &cache);
  v.global = Some(&global);

  assert_eq!(
    v.overrides_for("staging").expect("staging").args,
    vec!["--from-section".to_string()],
    "a section entry must win over the top-level one"
  );
  assert_eq!(
    v.overrides_for("prod").expect("prod").args,
    Vec::<String>::new(),
    "a top-level instance the section never mentions must still resolve"
  );

  // And an unknown name is measured against BOTH layers.
  let err = v.overrides_for("nosuch").expect_err("unknown");
  assert!(err.contains("prod, staging"), "must name every declared layer: {err}");
}

#[test]
fn instance_names_are_validated() {
  assert!(validate_instance_name("staging").is_ok());
  assert!(validate_instance_name("dev-2.eu_west").is_ok());
  assert!(validate_instance_name("").is_err());
  assert!(validate_instance_name(&"x".repeat(65)).is_err());
  // The name is caller-supplied and reaches a shell command.
  assert!(validate_instance_name("staging; rm -rf ~").is_err());
  assert!(validate_instance_name("$(whoami)").is_err());
  assert!(validate_instance_name("a/b").is_err());
}

#[test]
fn shell_form_quotes_the_instance_name() {
  // Even if a name slipped past validation, substitution is quoted, or
  // refused where the shell has no quoting that holds (cmd.exe).
  let s = spec(r#""echo --env ${INSTANCE}""#);
  let r = resolve_for_instance(&s, "sta'ging");
  #[cfg(unix)]
  assert_eq!(
    r.expect("resolve").exec,
    ResolvedExec::Shell(r"echo --env 'sta'\''ging'".to_string())
  );
  #[cfg(windows)]
  assert!(r.expect_err("refused").contains("argv form"));
}

#[test]
fn argv_form_never_involves_a_shell() {
  let s = spec(r#"{"run":["devgate","browser","args","--env","${INSTANCE}"]}"#);
  let r = resolve_for_instance(&s, "staging").expect("resolve");
  assert_eq!(
    r.exec,
    ResolvedExec::Argv(vec![
      "devgate".into(),
      "browser".into(),
      "args".into(),
      "--env".into(),
      "staging".into()
    ])
  );
}

#[test]
fn args_command_json_object_shape_is_accepted() {
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  let launch = r#"{"environment":"staging","args":["--a","--b"]}"#;
  let s = shell(
    &format!("printf '{launch}'"),
    &format!("echo {launch}"),
    serde_json::json!({}),
  );
  let v = view(&instances, Some(&s), None, &cache);
  assert_eq!(v.overrides_for("staging").expect("overrides").args, ["--a", "--b"]);
}

#[test]
fn args_command_line_output_is_accepted() {
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  let s = spec(r#""echo --one && echo --two""#);
  let v = view(&instances, Some(&s), None, &cache);
  assert_eq!(v.overrides_for("dev").expect("overrides").args, ["--one", "--two"]);
}

#[test]
fn args_command_failure_is_fatal_not_a_warning() {
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  let s = shell(
    "echo bad env >&2; exit 2",
    "echo bad env 1>&2 & exit 2",
    serde_json::json!({}),
  );
  let v = view(&instances, Some(&s), None, &cache);
  // Silently launching a browser with no environment mapping is how a
  // caller ends up on the wrong environment.
  let err = v.overrides_for("nope").expect_err("must fail");
  assert!(err.contains("exit"), "{err}");
  let health = v.health("nope").expect_err("must fail");
  assert!(health.contains("<env>:<context>"), "{health}");
}

#[test]
fn command_timeout_is_bounded() {
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  let s = shell(
    "sleep 5",
    "ping -n 6 127.0.0.1 >nul",
    serde_json::json!({ "timeoutMs": 150 }),
  );
  let v = view(&instances, Some(&s), None, &cache);
  let started = Instant::now();
  let err = v.overrides_for("slow").expect_err("must time out");
  assert!(err.contains("timed out"), "{err}");
  assert!(
    started.elapsed() < Duration::from_secs(2),
    "must not wait out the sleep"
  );
}

#[test]
fn per_instance_settings_are_resolved() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "staging".to_string(),
    InstanceConfig {
      args: vec!["--static".into()],
      user_data_dir: Some("/profiles/${INSTANCE}".into()),
      executable_path: Some("/bin/chrome".into()),
      headless: Some(true),
      env: BTreeMap::from([("APP_ENV".to_string(), "staging".to_string())]),
      proxy: Some(ProxyConfig {
        server: "http://localhost:3003".into(),
        bypass: Some("localhost".into()),
        ..Default::default()
      }),
      ignore_default_args: Some(IgnoreDefaultArgsConfig::Only(vec!["--no-sandbox".into()])),
      ..Default::default()
    },
  );
  let cache = CommandCache::default();
  let v = view(&instances, None, None, &cache);
  let o = v.overrides_for("staging").expect("overrides");

  assert_eq!(o.user_data_dir.as_deref(), Some("/profiles/staging"));
  assert_eq!(o.executable_path.as_deref(), Some("/bin/chrome"));
  assert_eq!(o.headless, Some(true));
  assert_eq!(o.env.get("APP_ENV").map(String::as_str), Some("staging"));
  assert!(o.args.contains(&"--static".to_string()));
  assert!(o.args.contains(&"--proxy-server=http://localhost:3003".to_string()));
  assert!(o.args.contains(&"--proxy-bypass-list=localhost".to_string()));
  assert_eq!(
    o.ignore_default_args,
    Some(ferridriver::options::IgnoreDefaultArgs::Some(vec![
      "--no-sandbox".into()
    ])),
    "ignoreDefaultArgs must reach the launch path"
  );
}

#[test]
fn an_instance_args_command_replaces_the_section_one() {
  let cache = CommandCache::default();
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "webkit".to_string(),
    InstanceConfig {
      args_command: Some(spec(r#""echo --from-instance""#)),
      ..Default::default()
    },
  );

  // Declared, but with no command of its own: it falls back to the section's.
  instances.insert("chrome".to_string(), InstanceConfig::default());

  let section = spec(r#""echo --from-section""#);
  let view = view(&instances, Some(&section), None, &cache);
  let out = view.overrides_for("webkit").expect("overrides");

  assert_eq!(
    out.args,
    vec!["--from-instance".to_string()],
    "instance command must win outright"
  );

  let other = view.overrides_for("chrome").expect("overrides");
  assert_eq!(other.args, vec!["--from-section".to_string()]);
}

#[test]
fn a_webkit_instance_is_never_discovered() {
  let cache = CommandCache::default();
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "webkit".to_string(),
    InstanceConfig {
      backend: Some(BackendChoice::WebKit),
      ..Default::default()
    },
  );

  // The command leaves a trace, so the test observes whether it RAN rather
  // than whether its endpoint was accepted (a dead one is rejected either way).
  let tmp = tempfile::tempdir().expect("tempdir");
  let marker = tmp.path().join("${INSTANCE}.ran");
  let discover = shell(
    &format!(
      "touch {} && echo ws://127.0.0.1:9222/devtools/browser/x",
      marker.display()
    ),
    &format!(
      r#"type nul > "{}" && echo ws://127.0.0.1:9222/devtools/browser/x"#,
      marker.display()
    ),
    serde_json::json!({}),
  );
  let view = view(&instances, None, Some(&discover), &cache);

  let _ = view.resolve_connect("webkit");
  assert!(
    !tmp.path().join("webkit.ran").exists(),
    "a WebKit browser exposes no endpoint to attach to, so discovery must not run"
  );

  let _ = view.resolve_connect("chrome");
  assert!(
    tmp.path().join("chrome.ran").exists(),
    "the section's discover command still serves CDP instances"
  );
}

#[test]
fn webkit_takes_its_own_proxy_switch() {
  let proxy = ProxyConfig {
    server: "http://127.0.0.1:3052".into(),
    bypass: Some("127.0.0.1,localhost".into()),
    username: None,
    password: None,
  };

  let chromium = proxy_launch_flags(&proxy.server, proxy.bypass.as_deref(), BackendKind::CdpPipe);
  assert!(chromium.contains(&"--proxy-server=http://127.0.0.1:3052".to_string()));

  // `--proxy-server` is not a WebKit switch: it is accepted and ignored, so
  // lowering it the Chromium way left WebKit talking to the network direct.
  let webkit = proxy_launch_flags(&proxy.server, proxy.bypass.as_deref(), BackendKind::WebKit);
  assert!(webkit.contains(&"--proxy=http://127.0.0.1:3052".to_string()));
  assert!(!webkit.iter().any(|arg| arg.starts_with("--proxy-server=")));

  if cfg!(target_os = "linux") {
    assert!(webkit.contains(&"--ignore-host=127.0.0.1".to_string()));
    assert!(webkit.contains(&"--ignore-host=localhost".to_string()));
  } else {
    assert!(webkit.contains(&"--proxy-bypass-list=127.0.0.1,localhost".to_string()));
  }
}

#[test]
fn an_instance_backend_overrides_the_sections_for_proxy_flags() {
  let cfg = InstanceConfig {
    backend: Some(BackendChoice::WebKit),
    proxy: Some(ProxyConfig {
      server: "http://127.0.0.1:3052".into(),
      bypass: None,
      username: None,
      password: None,
    }),
    ..Default::default()
  };

  let overrides = instance_overrides_from(&cfg, "staging", BackendKind::CdpPipe).expect("overrides");
  assert!(overrides.args.contains(&"--proxy=http://127.0.0.1:3052".to_string()));
}

#[test]
fn proxy_credentials_reach_the_engine_rather_than_being_dropped() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "p".to_string(),
    InstanceConfig {
      proxy: Some(ProxyConfig {
        server: "http://p:1".into(),
        username: Some("u".into()),
        password: Some("secret".into()),
        ..Default::default()
      }),
      ..Default::default()
    },
  );
  let cache = CommandCache::default();
  let v = view(&instances, None, None, &cache);
  let overrides = v
    .overrides_for("p")
    .expect("an instance proxy with credentials launches");
  let proxy = overrides.launched_proxy.expect("the proxy reaches the engine");
  assert_eq!(
    proxy.credentials().map(|c| (c.username, c.password)),
    Some(("u".into(), "secret".into()))
  );
  assert!(
    overrides.args.iter().all(|arg| !arg.contains("secret")),
    "a credential is never a flag: {:?}",
    overrides.args
  );
}

#[test]
fn android_instance_resolves_without_an_endpoint_or_device_identifier() {
  let cfg: InstanceConfig = serde_json::from_value(serde_json::json!({
    "device": {"platform":"android", "apiLevel":35, "model":"pixel_7"}, "headless":true
  }))
  .unwrap();
  let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
  let cache = CommandCache::default();
  let routing = view(&instances, None, None, &cache);
  routing.health("phone").unwrap();
  assert_eq!(
    routing.resolve_connect("phone"),
    Some(ConnectMode::Device {
      target: ferridriver::android::DeviceTarget::Android(ferridriver::android::AndroidOptions::default()),
      headless: true,
    })
  );
}

#[test]
fn android_package_selection_preserves_browser_arguments() {
  let cfg: InstanceConfig = serde_json::from_value(serde_json::json!({
    "args":["--enable-features=WebMCP"],
    "device":{"platform":"android", "pkg":"org.chromium.chrome", "apkPath":"browser.apk"}
  }))
  .unwrap();
  let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
  let cache = CommandCache::default();
  let routing = view(&instances, None, None, &cache);
  routing.health("phone").unwrap();
  assert_eq!(
    routing.overrides_for("phone").unwrap().args,
    ["--enable-features=WebMCP"]
  );
  let Some(ConnectMode::Device {
    target: ferridriver::device::DeviceTarget::Android(options),
    ..
  }) = routing.resolve_connect("phone")
  else {
    panic!("expected an Android target");
  };
  assert_eq!(options.pkg, "org.chromium.chrome");
  assert_eq!(options.apk_path, Some(std::path::PathBuf::from("browser.apk")));
}

#[test]
fn ios_rejects_browser_arguments_it_cannot_apply() {
  let cfg: InstanceConfig = serde_json::from_value(serde_json::json!({
    "args":["--enable-features=WebMCP"], "device":{"platform":"ios"}
  }))
  .unwrap();
  let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
  let cache = CommandCache::default();
  assert!(view(&instances, None, None, &cache).health("phone").is_err());
}

#[test]
fn mobile_targets_select_their_browser_and_backend() {
  for (platform, browser, backend) in [
    (
      "android",
      ferridriver::options::BrowserKind::Chromium,
      BackendKind::CdpWs,
    ),
    ("ios", ferridriver::options::BrowserKind::Safari, BackendKind::WebDriver),
  ] {
    let cfg: InstanceConfig = serde_json::from_value(serde_json::json!({"device":{"platform":platform}})).unwrap();
    let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    let routing = view(&instances, None, None, &cache);
    let overrides = routing.overrides_for("phone").unwrap();
    assert_eq!(overrides.kind, Some(browser));
    assert_eq!(overrides.backend, Some(backend));
    assert!(matches!(
      routing.resolve_connect("phone"),
      Some(ConnectMode::Device { .. })
    ));
  }
}

#[test]
fn mobile_targets_reject_conflicting_browser_choices() {
  for platform in ["android", "ios"] {
    let cfg: InstanceConfig =
      serde_json::from_value(serde_json::json!({"browser":"firefox","device":{"platform":platform}})).unwrap();
    let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    assert!(view(&instances, None, None, &cache).overrides_for("phone").is_err());
  }
}

#[test]
fn android_instance_rejects_conflicting_desktop_or_remote_settings() {
  for field in ["connectUrl", "discoverProfile", "userDataDir", "executablePath"] {
    let mut value = serde_json::json!({"device":{"platform":"android"}});
    value[field] = serde_json::json!("example");
    let cfg: InstanceConfig = serde_json::from_value(value).unwrap();
    let instances: rustc_hash::FxHashMap<_, _> = [("phone".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    assert!(view(&instances, None, None, &cache).health("phone").is_err(), "{field}");
  }
}

#[test]
fn instance_connection_options_select_a_managed_remote_session() {
  let cfg: InstanceConfig = serde_json::from_value(serde_json::json!({
    "connectUrl": "http://127.0.0.1:4725/wd/hub",
    "connectOptions": {
      "timeout": 120_000,
      "headers": {"authorization": "Bearer contract-token"},
      "capabilities": {
        "browserName": "Chrome",
        "platformName": "Android",
        "appium:options": {"automationName": "UiAutomator2", "deviceName": "example-device"}
      }
    }
  }))
  .unwrap();
  let instances: rustc_hash::FxHashMap<_, _> = [("remote".into(), cfg)].into_iter().collect();
  let cache = CommandCache::default();
  let routing = view(&instances, None, None, &cache);
  routing.health("remote").unwrap();
  let Some(ConnectMode::WebDriver {
    endpoint,
    browser_name,
    capabilities,
    headers,
    timeout,
    ..
  }) = routing.resolve_connect("remote")
  else {
    panic!("expected a managed WebDriver session");
  };
  assert_eq!(endpoint, "http://127.0.0.1:4725/wd/hub");
  assert_eq!(browser_name, "Chrome");
  assert_eq!(timeout, Some(120_000));
  assert_eq!(headers.unwrap()["authorization"], "Bearer contract-token");
  assert_eq!(capabilities.unwrap()["appium:options"]["deviceName"], "example-device");
}

#[test]
fn connection_options_on_a_websocket_url_keep_the_browsers_own_protocol() {
  for (url, backend) in [
    ("wss://grid.example.com/session/abc", None),
    ("ws://127.0.0.1:9222/session", Some(BackendChoice::Bidi)),
  ] {
    let cfg = InstanceConfig {
      backend,
      connect_url: Some(url.into()),
      connect_options: Some(ferridriver::options::ConnectOptions {
        headers: Some(
          [("authorization".to_string(), "Bearer grid-token".to_string())]
            .into_iter()
            .collect(),
        ),
        timeout: Some(5_000),
        ..Default::default()
      }),
      ..Default::default()
    };
    let instances: rustc_hash::FxHashMap<_, _> = [("grid".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    let Some(ConnectMode::ConnectUrlWith {
      url: endpoint,
      headers,
      timeout,
    }) = view(&instances, None, None, &cache).resolve_connect("grid")
    else {
      panic!("{url}: a WebSocket endpoint is not a WebDriver server");
    };
    assert_eq!(endpoint, url);
    assert_eq!(headers.unwrap()["authorization"], "Bearer grid-token");
    assert_eq!(timeout, Some(5_000));
  }
}

#[test]
fn remote_protocol_selection_follows_instance_and_section_precedence() {
  use ferridriver::backend::webdriver::WebDriverProtocol;
  for (section, browser, instance_backend, expected, name) in [
    (BackendKind::WebDriver, None, None, WebDriverProtocol::Classic, "chrome"),
    (BackendKind::Bidi, None, None, WebDriverProtocol::Bidi, "firefox"),
    (
      BackendKind::Bidi,
      None,
      Some(BackendKind::WebDriver),
      WebDriverProtocol::Classic,
      "chrome",
    ),
    (
      BackendKind::WebDriver,
      None,
      Some(BackendKind::Bidi),
      WebDriverProtocol::Bidi,
      "firefox",
    ),
    (
      BackendKind::Bidi,
      Some(ferridriver::options::BrowserKind::Safari),
      None,
      WebDriverProtocol::Auto,
      "safari",
    ),
    (
      BackendKind::WebDriver,
      Some(ferridriver::options::BrowserKind::Firefox),
      None,
      WebDriverProtocol::Auto,
      "firefox",
    ),
  ] {
    let cfg = InstanceConfig {
      browser,
      backend: instance_backend,
      connect_url: Some("http://127.0.0.1:4444".into()),
      connect_options: Some(ferridriver::options::ConnectOptions::default()),
      ..Default::default()
    };
    let instances: rustc_hash::FxHashMap<_, _> = [("remote".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    let global = BrowserSectionConfig {
      backend: Some(BackendKind::Bidi),
      ..Default::default()
    };
    let mut routing = view(&instances, None, None, &cache);
    routing.backend = section;
    routing.global = Some(&global);
    let Some(ConnectMode::WebDriver {
      protocol, browser_name, ..
    }) = routing.resolve_connect("remote")
    else {
      panic!("expected WebDriver connection");
    };
    assert_eq!(protocol, expected);
    assert_eq!(browser_name, name);
  }
}

#[test]
fn remote_instance_backend_resets_the_inherited_browser() {
  use ferridriver::options::BrowserKind;
  for (inherited, backend, expected) in [
    (BrowserKind::Chromium, BackendKind::Bidi, "firefox"),
    (BrowserKind::Firefox, BackendKind::WebDriver, "chrome"),
  ] {
    let cfg = InstanceConfig {
      backend: Some(backend),
      connect_url: Some("http://127.0.0.1:4444".into()),
      connect_options: Some(ferridriver::options::ConnectOptions::default()),
      ..Default::default()
    };
    let instances: rustc_hash::FxHashMap<_, _> = [("remote".into(), cfg)].into_iter().collect();
    let cache = CommandCache::default();
    let mut routing = view(&instances, None, None, &cache);
    routing.browser = Some(inherited);
    routing.backend = inherited.default_backend();
    let Some(ConnectMode::WebDriver { browser_name, .. }) = routing.resolve_connect("remote") else {
      panic!("expected WebDriver connection");
    };
    assert_eq!(browser_name, expected);
  }
}

#[test]
fn connection_options_without_an_endpoint_do_not_launch_a_local_browser() {
  let cfg = InstanceConfig {
    connect_options: Some(ferridriver::options::ConnectOptions::default()),
    ..Default::default()
  };
  let instances: rustc_hash::FxHashMap<_, _> = [("remote".into(), cfg)].into_iter().collect();
  let cache = CommandCache::default();
  assert!(
    view(&instances, None, None, &cache)
      .health("remote")
      .unwrap_err()
      .contains("connectUrl")
  );
}

#[test]
fn connect_url_wins_over_discovery() {
  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "remote".to_string(),
    InstanceConfig {
      connect_url: Some("ws://192.168.1.50:9222/devtools/browser/abc".into()),
      ..Default::default()
    },
  );
  let cache = CommandCache::default();
  let discover = spec(r#""echo ws://127.0.0.1:1/x""#);
  let v = view(&instances, None, Some(&discover), &cache);
  assert!(matches!(
    v.resolve_connect("remote"),
    Some(ConnectMode::ConnectUrl(url)) if url.contains("192.168.1.50")
  ));
}

#[test]
fn stale_profile_falls_through_to_the_discover_command() {
  let _net = port_guard();
  let dir = tempfile::tempdir().expect("tempdir");
  // A port nothing is listening on: the profile is stale.
  let (_reservation, dead) = crate::test_support::dead_endpoint().expect("reserve dead endpoint");
  std::fs::write(
    dir.path().join("DevToolsActivePort"),
    format!("{dead}\n/devtools/browser/gone"),
  )
  .expect("write");

  let live = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
  let live_port = live.local_addr().expect("addr").port();

  let mut instances = rustc_hash::FxHashMap::default();
  instances.insert(
    "staging".to_string(),
    InstanceConfig {
      discover_profile: Some(dir.path().to_string_lossy().into_owned()),
      ..Default::default()
    },
  );
  let cache = CommandCache::default();
  let discover = spec(&format!(r#""echo ws://127.0.0.1:{live_port}/devtools/browser/new""#));
  let v = view(&instances, None, Some(&discover), &cache);

  // The old implementation returned None here and never ran the
  // discover command, so a restarted browser was unreachable.
  assert!(matches!(
    v.resolve_connect("staging"),
    Some(ConnectMode::ConnectUrl(url)) if url.contains(&live_port.to_string())
  ));
}

#[test]
fn dead_discovered_endpoint_is_rejected_and_evicted() {
  let _net = port_guard();
  let (_reservation, dead) = crate::test_support::dead_endpoint().expect("reserve dead endpoint");
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  let discover = spec(&format!(r#""echo ws://127.0.0.1:{dead}/x""#));
  let v = view(&instances, None, Some(&discover), &cache);
  assert!(v.resolve_connect("any").is_none());
  // Evicted, so a browser that comes up later is found rather than
  // being masked for the whole TTL.
  let resolved = resolve_for_instance(&discover, "any").expect("resolve");
  assert!(
    cache.get_or_exec(&resolved, Duration::from_secs(0)).is_ok(),
    "entry must not be a cached failure"
  );
}

/// A command writing more than a pipe buffer used to block in
/// `write(2)` forever and be killed at the timeout, so a chatty
/// discover/args command looked like a hung one.
#[test]
fn output_larger_than_a_pipe_buffer_does_not_time_out() {
  let instances = rustc_hash::FxHashMap::default();
  let cache = CommandCache::default();
  // ~256KB of args, four times the usual 64KB pipe buffer.
  let s = shell(
    "for i in $(seq 1 16000); do echo --flag-$i; done",
    "for /l %i in (1,1,16000) do @echo --flag-%i",
    serde_json::json!({ "timeoutMs": 20000 }),
  );
  let v = view(&instances, Some(&s), None, &cache);
  let args = v.overrides_for("chatty").expect("must not time out").args;
  assert_eq!(args.len(), 16000, "every line survives");
  assert_eq!(args[15999], "--flag-16000");
}

/// The same command line from two directories is two commands: a
/// repo-relative helper resolves differently in each.
#[test]
fn cache_entries_are_scoped_to_the_command_cwd() {
  let a = tempfile::tempdir().expect("tempdir");
  let b = tempfile::tempdir().expect("tempdir");
  std::fs::write(a.path().join("marker"), "A").expect("write");
  std::fs::write(b.path().join("marker"), "B").expect("write");

  let cache = CommandCache::default();
  let make = |dir: &Path| {
    let spec = shell("cat marker", "type marker", serde_json::json!({ "cwd": dir }));
    resolve_for_instance(&spec, "x").expect("resolve")
  };
  let first = cache.get_or_exec(&make(a.path()), DEFAULT_CACHE_TTL).expect("a");
  let second = cache.get_or_exec(&make(b.path()), DEFAULT_CACHE_TTL).expect("b");
  assert_eq!(first, serde_json::Value::String("A".into()));
  assert_eq!(second, serde_json::Value::String("B".into()), "cwd is part of identity");
}

/// Concurrent cold callers must not each spawn the command: a discover
/// command that polls would cost one full poll per caller.
#[test]
fn concurrent_misses_run_the_command_once() {
  let dir = tempfile::tempdir().expect("tempdir");
  let counter = dir.path().join("runs");
  let cache = Arc::new(CommandCache::default());
  let counter_path = counter.display().to_string();
  let s = shell(
    &format!("echo x >> '{counter_path}'; wc -l < '{counter_path}'"),
    &format!(r#"echo x>>"{counter_path}" & find /c /v "" < "{counter_path}""#),
    serde_json::json!({}),
  );
  let resolved = resolve_for_instance(&s, "x").expect("resolve");

  std::thread::scope(|scope| {
    for _ in 0..8 {
      let cache = Arc::clone(&cache);
      let resolved = resolved.clone();
      scope.spawn(move || {
        cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("exec");
      });
    }
  });

  let runs = std::fs::read_to_string(&counter).expect("counter").lines().count();
  assert_eq!(runs, 1, "eight concurrent misses, one execution");
}

#[test]
fn cache_serves_within_ttl_and_flush_clears_it() {
  let cache = CommandCache::default();
  let s = shell("date +%s%N", "echo %RANDOM%%TIME%", serde_json::json!({}));
  let resolved = resolve_for_instance(&s, "x").expect("resolve");
  let first = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("first");
  let second = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("second");
  assert_eq!(first, second, "served from cache");
  cache.flush();
  let third = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("third");
  assert_ne!(first, third, "flush must force a re-run");
}

#[test]
fn discover_command_output_shapes() {
  assert_eq!(
    first_ws_url(&serde_json::json!("ws://a:1/x")).as_deref(),
    Some("ws://a:1/x")
  );
  assert_eq!(
    first_ws_url(&serde_json::json!({"wsEndpoint": "wss://b:2/y"})).as_deref(),
    Some("wss://b:2/y")
  );
  assert_eq!(
    first_ws_url(&serde_json::json!(["http://c", "ws://d:3/z"])).as_deref(),
    Some("ws://d:3/z")
  );
  // A snake_case key is what a Rust/Go discover command emits by default;
  // missing it made a found browser look like no browser.
  assert_eq!(
    first_ws_url(&serde_json::json!({"env": "staging", "ws_url": "ws://e:4/w", "port": 4})).as_deref(),
    Some("ws://e:4/w")
  );
  assert_eq!(
    first_ws_url(&serde_json::json!({"wsUrl": "ws://f:5/v"})).as_deref(),
    Some("ws://f:5/v")
  );
  assert!(first_ws_url(&serde_json::json!("not-a-url")).is_none());
  assert!(
    first_ws_url(&serde_json::json!({"port": 9222})).is_none(),
    "a payload with no ws URL is still a miss"
  );
}
