//! Unit tests for `BrowserState`, launch plans, discovery, and visited origins.

use super::sessions::*;
use super::*;

pub(super) use state_tests::test_state;

#[cfg(test)]
mod discovery_tests {
  use super::{discover_ws_from_http, parse_devtools_active_port};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  /// Serve one hand-rolled HTTP response, then hold the socket open.
  ///
  /// Keeping the connection alive is the whole point: Chrome 136+ ignores our
  /// `Connection: close`, so a reader that waits for EOF waits forever.
  ///
  /// The request head is read before answering: bytes that arrive while the
  /// client is still writing its request are an unexpected message to hyper,
  /// which cancels the request instead of reading the response.
  async fn serve_once(response: &'static str) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let mut head = Vec::new();
      let mut chunk = [0_u8; 1024];
      while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match sock.read(&mut chunk).await {
          Ok(0) | Err(_) => return,
          Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
      }
      let _ = sock.write_all(response.as_bytes()).await;
      let _ = sock.flush().await;
      // Never shut down the write half, and never drop `sock`.
      std::future::pending::<()>().await;
    });
    addr
  }

  /// Chrome 136+ with "Allow remote debugging for this browser instance" binds
  /// the port but serves no HTTP JSON endpoints. It answers a bodyless 404 and
  /// leaves the connection intact, which used to park the read forever and
  /// wedge the global state lock behind it.
  #[tokio::test]
  async fn bodyless_keepalive_404_errors_instead_of_hanging() {
    let addr = serve_once("HTTP/1.1 404 Not Found\r\nContent-Length:0\r\nContent-Type:text/html\r\n\r\n").await;

    let result = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      discover_ws_from_http(&format!("http://{addr}")),
    )
    .await
    .expect("discovery must not hang on a keep-alive 404");

    let err = result.expect_err("404 is not a successful discovery").to_string();
    assert!(err.contains("404"), "error should name the status: {err}");
    assert!(
      err.contains("DevToolsActivePort"),
      "error should point at the workaround: {err}"
    );
  }

  /// A 200 whose `Content-Length` is honoured exactly, on a connection that
  /// never closes. Reading to EOF here would hang just as badly as the 404.
  #[tokio::test]
  async fn keepalive_200_is_read_by_content_length() {
    let body = r#"{"webSocketDebuggerUrl":"ws://localhost:9222/devtools/browser/abc"}"#;
    let response: &'static str =
      Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str());
    let addr = serve_once(response).await;

    let ws = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      discover_ws_from_http(&format!("http://{addr}")),
    )
    .await
    .expect("discovery must not hang")
    .expect("valid /json/version");

    // The advertised `localhost` authority is replaced by the address actually reached.
    assert_eq!(ws, format!("ws://{addr}/devtools/browser/abc"));
  }

  #[test]
  fn devtools_active_port_needs_both_lines() {
    assert_eq!(
      parse_devtools_active_port("9222\n/devtools/browser/abc\n").expect("two lines"),
      "ws://127.0.0.1:9222/devtools/browser/abc"
    );
    assert!(parse_devtools_active_port("9222\n").is_err());
    assert!(parse_devtools_active_port("").is_err());
    assert!(parse_devtools_active_port("notaport\n/devtools/browser/abc").is_err());
  }
}

#[cfg(test)]
mod state_tests {
  use std::sync::Arc;

  use super::*;

  #[test]
  fn pin_ws_authority_rewrites_advertised_localhost_host() {
    let addr = "127.0.0.1:9222".parse().unwrap();
    // Chrome advertises localhost; the actual listener is 127.0.0.1.
    assert_eq!(
      pin_ws_authority("ws://localhost:9222/devtools/browser/abc-123", addr),
      "ws://127.0.0.1:9222/devtools/browser/abc-123"
    );
    // Path-less and wss variants are handled; non-ws input is untouched.
    assert_eq!(pin_ws_authority("ws://localhost:9222", addr), "ws://127.0.0.1:9222");
    assert_eq!(
      pin_ws_authority("wss://localhost:9222/x", addr),
      "wss://127.0.0.1:9222/x"
    );
    assert_eq!(
      pin_ws_authority("http://localhost:9222/x", addr),
      "http://localhost:9222/x"
    );
  }

  #[test]
  fn pin_ws_authority_preserves_ipv6_literal() {
    let addr: std::net::SocketAddr = "[::1]:9222".parse().unwrap();
    assert_eq!(
      pin_ws_authority("ws://localhost:9222/devtools/browser/x", addr),
      "ws://[::1]:9222/devtools/browser/x"
    );
  }
  use crate::backend::BackendKind;

  /// Test helper: build a `BrowserState` with the minimum `LaunchPlan`
  /// needed to exercise the resolver/args plumbing. Using
  /// `LaunchPlan::default()` keeps these tests in lock-step with the
  /// single production construction path ([`BrowserState::with_plan`]).
  pub(in crate::state) fn test_state(backend: BackendKind) -> BrowserState {
    let selection = crate::options::BrowserSelection::resolve(None, Some(backend)).expect("backend selection");
    BrowserState::with_plan(
      ConnectMode::Launch,
      crate::options::LaunchPlan {
        backend,
        kind: selection.browser,
        headless: true,
        ..Default::default()
      },
    )
  }

  #[tokio::test]
  async fn configured_safari_connection_uses_its_backend_in_both_state_paths() {
    use tokio::io::AsyncWriteExt;

    for shared in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for value in [
          serde_json::json!({"sessionId":"configured-safari","capabilities":{"browserName":"Safari","browserVersion":"26.2"}}),
          serde_json::json!(["device"]),
          serde_json::Value::Null,
        ] {
          let (mut socket, _) = listener.accept().await.unwrap();
          requests.push(crate::backend::webdriver::session::tests::request(&mut socket).await);
          let body = serde_json::json!({"value":value}).to_string();
          socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
      });
      let mut state = test_state(BackendKind::CdpPipe);
      state.set_instance_overrides_fn(Arc::new(|_| {
        Ok(crate::options::InstanceOverrides {
          kind: Some(crate::options::BrowserKind::Safari),
          ..Default::default()
        })
      }));
      state.set_instance_resolver_fn(Arc::new(move |_| {
        Some(ConnectMode::WebDriver {
          endpoint: endpoint.clone(),
          browser_name: "safari".into(),
          protocol: crate::backend::webdriver::WebDriverProtocol::Auto,
          capabilities: None,
          headers: None,
          timeout: Some(1000),
        })
      }));
      let state = state.into_shared();
      if shared {
        BrowserState::ensure_instance_shared(&state, "safari").await.unwrap();
      } else {
        state.write().await.ensure_instance("safari").await.unwrap();
      }
      assert!(matches!(
        state.read().await.instances["safari"].browser,
        AnyBrowser::WebDriver(_)
      ));
      state.write().await.shutdown().await.unwrap();
      let requests = server.await.unwrap();
      assert!(
        requests[0].1["capabilities"]["alwaysMatch"]
          .get("webSocketUrl")
          .is_none()
      );
      assert_eq!(requests[1].0, "GET /session/configured-safari/window/handles HTTP/1.1");
      assert_eq!(requests[2].0, "DELETE /session/configured-safari HTTP/1.1");
    }
  }

  #[tokio::test]
  async fn failed_or_cancelled_shutdown_retains_the_provider_for_retry() {
    use tokio::io::AsyncWriteExt as _;

    for cancelled in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let (received, receipt) = tokio::sync::oneshot::channel();
      let server = tokio::spawn(async move {
        let mut received = Some(received);
        let mut held = Vec::new();
        let mut requests = Vec::new();
        for (index, value) in [
          serde_json::json!({"sessionId":"cleanup","capabilities":{"browserName":"Safari","browserVersion":"26.2"}}),
          serde_json::json!(["page"]),
          serde_json::json!({"error":"unknown error","message":"provider cleanup unavailable"}),
          serde_json::Value::Null,
        ]
        .into_iter()
        .enumerate()
        {
          let (mut socket, _) = listener.accept().await.unwrap();
          requests.push(crate::backend::webdriver::session::tests::request(&mut socket).await);
          if index == 2 && cancelled {
            held.push(socket);
            received.take().unwrap().send(()).unwrap();
            continue;
          }
          let status = if index == 2 {
            "500 Internal Server Error"
          } else {
            "200 OK"
          };
          let body = serde_json::json!({"value":value}).to_string();
          socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
      });
      let mut state = test_state(BackendKind::WebDriver);
      state.connect_mode = ConnectMode::WebDriver {
        endpoint,
        browser_name: "safari".into(),
        protocol: crate::backend::webdriver::WebDriverProtocol::Classic,
        capabilities: None,
        headers: None,
        timeout: Some(1000),
      };
      state.ensure_instance("default").await.unwrap();
      if cancelled {
        tokio::select! {
          result = state.shutdown() => panic!("cleanup completed before cancellation: {result:?}"),
          result = receipt => result.unwrap(),
        }
      } else {
        assert!(
          state
            .shutdown()
            .await
            .unwrap_err()
            .to_string()
            .contains("provider cleanup unavailable")
        );
      }
      assert!(!state.is_connected());
      assert_eq!(state.pending_cleanup.len(), 1);
      assert!(
        state
          .ensure_instance("default")
          .await
          .unwrap_err()
          .to_string()
          .contains("unfinished cleanup")
      );
      let state = state.into_shared();
      assert!(
        BrowserState::ensure_instance_shared(&state, "default")
          .await
          .unwrap_err()
          .to_string()
          .contains("unfinished cleanup")
      );
      state.write().await.shutdown().await.unwrap();
      assert!(state.read().await.pending_cleanup.is_empty());
      state.write().await.shutdown().await.unwrap();
      let requests = server.await.unwrap();
      assert_eq!(requests.len(), 4);
      assert_eq!(requests[2].0, "DELETE /session/cleanup HTTP/1.1");
      assert_eq!(requests[3].0, requests[2].0);
    }
  }

  #[tokio::test]
  async fn webkit_browser_override_does_not_inherit_chromium_window_flags() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_overrides_fn(Arc::new(|_| {
      Ok(crate::options::InstanceOverrides {
        kind: Some(crate::options::BrowserKind::WebKit),
        ..Default::default()
      })
    }));
    let (_, effective) = state.launch_spec().resolve_off_lock("webkit").await.unwrap();
    assert_eq!(effective.backend_kind, BackendKind::WebKit);
    assert!(!effective.args.iter().any(|arg| arg.starts_with("--window-size")));
  }

  #[test]
  fn test_instance_resolver_none_by_default() {
    let state = test_state(BackendKind::CdpPipe);
    assert!(state.instance_resolver_fn.is_none());
  }

  #[test]
  fn test_instance_resolver_returns_connect_url() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_resolver_fn(Arc::new(|instance| match instance {
      "staging" => Some(ConnectMode::ConnectUrl(
        "ws://127.0.0.1:9222/devtools/browser/abc".to_owned(),
      )),
      _ => None,
    }));

    // Resolver returns Some for "staging"
    let resolved = state.instance_resolver_fn.as_ref().unwrap()("staging");
    assert!(matches!(resolved, Some(ConnectMode::ConnectUrl(url)) if url.contains("9222")));

    // Resolver returns None for unknown instance (falls through to default)
    let resolved = state.instance_resolver_fn.as_ref().unwrap()("unknown");
    assert!(resolved.is_none());
  }

  #[test]
  fn bare_session_key_selects_a_configured_instance() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_known_instances(["staging".to_string(), "dev".to_string()]);

    // The whole point: `session: "staging"` used to mean "a context
    // named staging on the default instance", i.e. a browser with no
    // environment mapping at all.
    let key = state.session_key("staging");
    assert_eq!(&*key.instance, "staging");
    assert_eq!(&*key.context, "default");

    // A name that is NOT an instance keeps the context meaning.
    let key = state.session_key("my-scratch-session");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "my-scratch-session");

    // Composite keys are unaffected.
    let key = state.session_key("staging:admin");
    assert_eq!(&*key.instance, "staging");
    assert_eq!(&*key.context, "admin");

    // `default` stays the default pair.
    let key = state.session_key("default");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "default");
  }

  /// The vocabulary is per-state, so two states in one process disagree
  /// about a bare key without either one leaking into the other — which
  /// a process-global registry could not express.
  #[test]
  fn the_instance_vocabulary_is_per_state() {
    let mut with_staging = test_state(BackendKind::CdpPipe);
    with_staging.set_known_instances(["staging".to_string()]);
    let plain = test_state(BackendKind::CdpPipe);

    assert_eq!(&*with_staging.session_key("staging").instance, "staging");
    assert_eq!(&*plain.session_key("staging").instance, "default");
    assert_eq!(&*plain.session_key("staging").context, "staging");
    // The bare, vocabulary-free parse keeps the context meaning too.
    assert_eq!(&*SessionKey::parse("staging").context, "staging");
  }

  #[test]
  fn test_instance_overrides_fn_independent_of_resolver() {
    let mut state = test_state(BackendKind::CdpPipe);

    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec![format!("--window-name={instance}")],
        user_data_dir: Some(format!("/profiles/{instance}")),
        headless: Some(true),
        ..Default::default()
      })
    }));

    state.set_instance_resolver_fn(Arc::new(|_| None));

    // Both callbacks set independently
    let overrides = state.instance_overrides_fn.as_ref().unwrap()("dev").expect("overrides");
    assert_eq!(overrides.args, vec!["--window-name=dev"]);
    assert_eq!(overrides.user_data_dir.as_deref(), Some("/profiles/dev"));
    assert_eq!(overrides.headless, Some(true));

    let resolved = state.instance_resolver_fn.as_ref().unwrap()("dev");
    assert!(resolved.is_none());
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn per_instance_overrides_reach_the_effective_launch() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.headless = false;
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec!["--instance-flag".into()],
        user_data_dir: Some(format!("/profiles/{instance}")),
        executable_path: Some("/bin/other-chrome".into()),
        headless: Some(true),
        backend: Some(BackendKind::CdpWs),
        kind: None,
        env: [("APP_ENV".to_string(), instance.to_string())].into_iter().collect(),
        ignore_default_args: Some(crate::options::IgnoreDefaultArgs::Some(vec!["--no-sandbox".into()])),
        launched_proxy: Some(crate::options::ProxyConfig {
          server: "http://127.0.0.1:3128".into(),
          username: Some("user".into()),
          password: Some("secret".into()),
          bypass: None,
        }),
      })
    }));

    let spec = state.launch_spec();
    let (mode, eff) = spec.resolve_off_lock("staging").await.expect("resolve");
    assert_eq!(
      eff
        .launched_proxy
        .as_ref()
        .and_then(crate::options::ProxyConfig::credentials)
        .map(|c| c.username),
      Some("user".to_string()),
      "the instance's proxy credentials reach the launch"
    );
    assert!(eff.proxy.is_none(), "the instance's own flags already name its proxy");

    assert!(matches!(mode, ConnectMode::Launch));
    assert!(eff.args.contains(&"--instance-flag".to_string()));
    assert_eq!(eff.user_data_dir.as_deref(), Some("/profiles/staging"));
    assert_eq!(eff.chromium_path, "/bin/other-chrome");
    assert!(eff.headless, "instance override beats the state default");
    assert_eq!(eff.backend_kind, BackendKind::CdpWs);
    assert_eq!(eff.env.get("APP_ENV").map(String::as_str), Some("staging"));

    // `ignoreDefaultArgs` must actually drop the switch.
    let flags = chrome_flags_with(eff.headless, &eff.args, eff.ignore_default_args.as_ref(), false);
    assert!(!flags.iter().any(|f| f == "--no-sandbox"), "dropped: {flags:?}");
    assert!(flags.iter().any(|f| f == "--instance-flag"), "user args survive");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn overrides_callback_error_aborts_the_launch() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Err(format!("no environment mapped for '{instance}'"))
    }));

    let spec = state.launch_spec();
    // Silently launching an unconfigured browser is the failure this
    // prevents: the caller would be on the wrong environment.
    let err = spec.resolve_off_lock("bogus").await.expect_err("must abort");
    assert!(err.to_string().contains("no environment mapped"), "{err}");
  }

  #[test]
  fn chromium_sandbox_true_keeps_the_sandbox() {
    assert!(
      chrome_flags_with(true, &[], None, false)
        .iter()
        .any(|f| f == "--no-sandbox")
    );
    assert!(
      !chrome_flags_with(true, &[], None, true)
        .iter()
        .any(|f| f == "--no-sandbox")
    );
    assert!(
      chrome_flags_with(true, &["--no-sandbox".to_string()], None, true)
        .iter()
        .any(|f| f == "--no-sandbox"),
      "an explicit argument still wins"
    );
  }

  #[test]
  fn ignore_default_args_all_drops_every_builtin() {
    let flags = chrome_flags_with(
      false,
      &["--keep-me".to_string()],
      Some(&crate::options::IgnoreDefaultArgs::All),
      false,
    );
    assert_eq!(flags, vec!["--keep-me"]);
  }

  #[test]
  fn loading_an_extension_keeps_headless_defaults_and_removes_only_extension_blocker() {
    let flags = chrome_flags_with(
      true,
      &["--load-extension=/tmp/example-extension".to_string()],
      None,
      false,
    );
    assert!(
      flags
        .iter()
        .any(|flag| flag == "--load-extension=/tmp/example-extension")
    );
    assert!(!flags.iter().any(|flag| flag == "--disable-extensions"));
    assert!(flags.iter().any(|flag| flag == "--headless"));
    assert!(flags.iter().any(|flag| flag == "--enable-automation"));
  }

  /// The headless switches are part of Playwright's `defaultArgs()`, so
  /// `ignoreDefaultArgs: true` drops them too. Gating only the other
  /// sections left a caller who asked for no defaults with a browser
  /// still forced headless and still colour-scheme-pinned.
  #[test]
  fn ignore_default_args_all_drops_the_headless_switches_too() {
    let flags = chrome_flags_with(
      true,
      &["--keep-me".to_string()],
      Some(&crate::options::IgnoreDefaultArgs::All),
      false,
    );
    assert_eq!(flags, vec!["--keep-me"], "got: {flags:?}");
  }

  #[test]
  fn a_named_headless_switch_can_be_dropped_on_its_own() {
    let flags = chrome_flags_with(
      true,
      &[],
      Some(&crate::options::IgnoreDefaultArgs::Some(vec![
        "--hide-scrollbars".to_string(),
      ])),
      false,
    );
    assert!(!flags.iter().any(|f| f == "--hide-scrollbars"), "got: {flags:?}");
    assert!(flags.iter().any(|f| f == "--headless"), "the rest survive");
  }

  /// `ignoreDefaultArgs` filters a Chromium switch list that the Firefox
  /// and `WebKit` launch paths do not have, so accepting it there and
  /// applying it nowhere would tell the caller a switch was dropped when
  /// nothing was.
  #[tokio::test(flavor = "multi_thread")]
  async fn ignore_default_args_is_refused_on_the_backends_without_defaults() {
    for backend in [BackendKind::Bidi, BackendKind::WebKit] {
      let mut state = test_state(backend);
      state.set_instance_overrides_fn(Arc::new(|_| {
        Ok(crate::options::InstanceOverrides {
          ignore_default_args: Some(crate::options::IgnoreDefaultArgs::All),
          ..Default::default()
        })
      }));
      let spec = state.launch_spec();
      let (_, eff) = spec.resolve_off_lock("x").await.expect("resolve");
      let Err(err) = spec.launch_browser(&eff).await else {
        panic!("{backend:?} must refuse ignoreDefaultArgs rather than drop it");
      };
      assert!(
        matches!(err, FerriError::Unsupported { .. }),
        "{backend:?} must report Unsupported, got {err}"
      );
      assert!(err.to_string().contains("ignoreDefaultArgs"), "{err}");
    }
  }

  /// An instance's profile directory and args have to reach EVERY
  /// backend's launch config, not just the Chromium ones — a config that
  /// is accepted, validated and then dropped is worse than a rejected
  /// one.
  #[tokio::test(flavor = "multi_thread")]
  async fn webkit_carries_the_instance_profile_and_args() {
    let mut state = test_state(BackendKind::WebKit);
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec!["--instance-flag".into()],
        user_data_dir: Some(format!("/profiles/{instance}")),
        ..Default::default()
      })
    }));
    let spec = state.launch_spec();
    let (_, eff) = spec.resolve_off_lock("staging").await.expect("resolve");

    // The launch itself needs a real WebKit build, so assert on the
    // config the launch would be given.
    let config = crate::backend::webkit::LaunchConfig {
      headless: eff.headless,
      env: eff.env.clone(),
      user_data_dir: eff.user_data_dir.as_ref().map(std::path::PathBuf::from),
      extra_args: eff.args,
      ..Default::default()
    };
    assert_eq!(
      config.user_data_dir.as_deref(),
      Some(std::path::Path::new("/profiles/staging"))
    );
    assert!(config.extra_args.contains(&"--instance-flag".to_string()));
  }

  #[tokio::test]
  async fn managed_devices_keep_browser_arguments_without_desktop_window_flags() {
    let state = BrowserState::with_plan(
      ConnectMode::Device {
        target: crate::device::DeviceTarget::Android(crate::android::AndroidOptions::default()),
        headless: true,
      },
      crate::options::LaunchPlan {
        backend: BackendKind::CdpWs,
        args: vec!["--enable-features=WebMCP".into()],
        ..Default::default()
      },
    );
    let (mode, effective) = state.launch_spec().resolve_off_lock("phone").await.unwrap();
    assert!(matches!(mode, ConnectMode::Device { .. }));
    assert_eq!(effective.args, ["--enable-features=WebMCP"]);
  }

  #[test]
  fn feature_switches_merge_so_defaults_survive_and_the_caller_wins() {
    let switches = |prefix: &str, args: &[&str]| {
      let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
      chrome_flags(true, &args)
        .into_iter()
        .filter_map(|flag| flag.strip_prefix(prefix).map(str::to_owned))
        .collect::<Vec<_>>()
    };
    let enabled = |args: &[&str]| switches("--enable-features=", args);
    let disabled = |args: &[&str]| switches("--disable-features=", args);

    assert_eq!(enabled(&[]), ["CDPScreenshotNewSurface"]);
    assert_eq!(
      enabled(&["--enable-features=WebMCP"]),
      ["CDPScreenshotNewSurface,WebMCP,WebMCPTesting,DevToolsWebMCPSupport"]
    );
    assert_eq!(
      enabled(&["--enable-features=Foo,DevToolsWebMCPSupport,WebMCP"]),
      ["CDPScreenshotNewSurface,Foo,DevToolsWebMCPSupport,WebMCP,WebMCPTesting"]
    );
    assert_eq!(
      enabled(&["--enable-features=WebMCP", "--enable-features=Foo<Trial:sashoush/1"]),
      ["CDPScreenshotNewSurface,WebMCP,Foo<Trial:sashoush/1,WebMCPTesting,DevToolsWebMCPSupport"]
    );

    let defaults = disabled(&[]);
    assert_eq!(defaults.len(), 1);
    assert!(defaults[0].contains("HttpsUpgrades"), "{defaults:?}");
    assert_eq!(
      disabled(&["--disable-features=Sashoush"]),
      [format!("{},Sashoush", defaults[0])]
    );

    // A later mention wins: the caller can turn a default either way.
    assert!(!disabled(&["--enable-features=HttpsUpgrades"])[0].contains("HttpsUpgrades"));
    assert_eq!(
      enabled(&["--enable-features=HttpsUpgrades"]),
      ["CDPScreenshotNewSurface,HttpsUpgrades"]
    );
    assert_eq!(
      enabled(&["--disable-features=CDPScreenshotNewSurface"]),
      Vec::<String>::new()
    );
    let args = ["--enable-features=WebMCP", "--disable-features=WebMCPTesting"];
    assert_eq!(enabled(&args), ["CDPScreenshotNewSurface,WebMCP,DevToolsWebMCPSupport"]);
    assert!(disabled(&args)[0].ends_with(",WebMCPTesting"));
  }

  #[test]
  fn ignore_default_args_matches_on_switch_name() {
    // A default carrying a value is dropped by its bare name.
    let flags = chrome_flags_with(
      false,
      &[],
      Some(&crate::options::IgnoreDefaultArgs::Some(vec![
        "--disable-features".to_string(),
      ])),
      false,
    );
    assert!(
      !flags.iter().any(|f| f.starts_with("--disable-features")),
      "got: {flags:?}"
    );
    assert!(flags.iter().any(|f| f == "--no-sandbox"), "others survive");
  }

  #[tokio::test]
  async fn test_ensure_instance_uses_resolver_for_connect() {
    // Bind then drop to get a port that's definitely not listening.
    let port = {
      let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      l.local_addr().unwrap().port()
      // listener drops here, port is free
    };

    let mut state = test_state(BackendKind::CdpWs);
    state.set_instance_resolver_fn(Arc::new(move |instance| {
      if instance == "test-resolved" {
        Some(ConnectMode::ConnectUrl(format!(
          "ws://127.0.0.1:{port}/devtools/browser/test"
        )))
      } else {
        None
      }
    }));

    // Should attempt WebSocket connection to the dead port (fails fast with
    // "connection refused"), proving the resolver was invoked instead of launching.
    let result = Box::pin(state.ensure_instance("test-resolved")).await;
    assert!(
      result.is_err(),
      "Should fail with connection refused, proving resolver was invoked"
    );
    let err = result.unwrap_err().to_string();
    assert!(
      !err.contains("not found") && !err.contains("No such file"),
      "Error should be connection-related, not binary-not-found: {err}"
    );
  }

  #[tokio::test]
  async fn test_ensure_instance_skips_resolver_when_exists() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let call_count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&call_count);

    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_resolver_fn(Arc::new(move |_| {
      counter.fetch_add(1, Ordering::Relaxed);
      None // Fall through to default
    }));

    // First call: resolver should be called (but will fall through and try to launch)
    let _ = Box::pin(state.ensure_instance("test")).await;
    // Resolver was called exactly once (regardless of whether launch succeeded)
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
  }
}

#[cfg(test)]
mod visited_origin_tests {
  use super::VisitedOrigins;

  #[test]
  fn only_http_origins_are_kept_per_context() {
    let visited = VisitedOrigins::default();
    visited.visit("a", "https://example.com/login?next=/");
    visited.visit("a", "http://127.0.0.1:8080/x");
    visited.visit("a", "https://example.com/other");
    visited.visit("a", "about:blank");
    visited.visit("a", "data:text/html,<p>x</p>");
    visited.visit("a", "file:///tmp/report.html");
    visited.visit("b", "https://acme.test/");
    assert_eq!(visited.of("a"), ["http://127.0.0.1:8080", "https://example.com"]);
    assert_eq!(visited.of("b"), ["https://acme.test"]);
    assert_eq!(visited.of("c"), Vec::<String>::new());

    visited.replace("a", ["https://acme.test".to_string()]);
    assert_eq!(visited.of("a"), ["https://acme.test"]);
    visited.forget("a");
    assert_eq!(visited.of("a"), Vec::<String>::new());
    assert_eq!(visited.of("b"), ["https://acme.test"]);
  }
}
