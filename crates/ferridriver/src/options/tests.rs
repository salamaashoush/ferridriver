//! Unit tests for `options`.

use super::*;

#[cfg(test)]
mod highlight_style_tests {
  use super::*;

  #[test]
  fn css_string_passes_through_verbatim() {
    let s = HighlightStyle::Css("outline: 2px solid red".to_string());
    assert_eq!(s.to_css_string(), "outline: 2px solid red");
  }

  #[test]
  fn object_camel_case_becomes_kebab_case() {
    let s = HighlightStyle::Object(vec![("backgroundColor".to_string(), "red".to_string())]);
    assert_eq!(s.to_css_string(), "background-color: red");
  }

  #[test]
  fn object_custom_property_preserved() {
    let s = HighlightStyle::Object(vec![("--my-var".to_string(), "10".to_string())]);
    assert_eq!(s.to_css_string(), "--my-var: 10");
  }

  #[test]
  fn object_multiple_entries_joined_with_semicolon() {
    let s = HighlightStyle::Object(vec![
      ("border".to_string(), "1px".to_string()),
      ("zIndex".to_string(), "5".to_string()),
    ]);
    assert_eq!(s.to_css_string(), "border: 1px; z-index: 5");
  }

  #[test]
  fn from_str_is_css_variant() {
    assert!(matches!(HighlightStyle::from("a: b"), HighlightStyle::Css(_)));
  }
}

#[cfg(test)]
mod pdf_option_tests {
  use super::*;

  // ── PdfSize parsing ──────────────────────────────────────────────────

  #[test]
  fn parses_pixel_suffix() {
    assert_eq!(PdfSize::parse("100px").unwrap(), PdfSize::Pixels(100.0));
  }

  #[test]
  fn parses_inch_suffix() {
    assert_eq!(PdfSize::parse("8.5in").unwrap(), PdfSize::Inches(8.5));
  }

  #[test]
  fn parses_cm_and_mm_suffixes() {
    assert_eq!(PdfSize::parse("10cm").unwrap(), PdfSize::Centimeters(10.0));
    assert_eq!(PdfSize::parse("5.5mm").unwrap(), PdfSize::Millimeters(5.5));
  }

  #[test]
  fn parses_suffix_case_insensitively() {
    assert_eq!(PdfSize::parse("8.5IN").unwrap(), PdfSize::Inches(8.5));
    assert_eq!(PdfSize::parse("100Px").unwrap(), PdfSize::Pixels(100.0));
  }

  #[test]
  fn multibyte_suffix_is_a_parse_error_not_a_panic() {
    assert!(PdfSize::parse("1\u{20ac}").is_err());
  }

  #[test]
  fn bare_number_falls_back_to_pixels() {
    // parity: Phantom-compatible fallback to px if no known unit.
    assert_eq!(PdfSize::parse("42").unwrap(), PdfSize::Pixels(42.0));
  }

  #[test]
  fn unknown_suffix_falls_back_to_pixels() {
    // `em` is not in the table — treats the whole string as px.
    // The numeric value here is "42" (with "em" treated as suffix but then
    // falling through to the default "px" branch). The parser slices the
    // last 2 chars, sees "em" (unknown), then parses the WHOLE original
    // string as a number. "42em" isn't a number → error. Unknown suffix
    // + non-numeric body ⇒ InvalidArgument.
    assert!(PdfSize::parse("42em").is_err());
  }

  #[test]
  fn invalid_number_is_rejected() {
    assert!(PdfSize::parse("abcpx").is_err());
  }

  #[test]
  fn short_input_takes_pixel_fallback() {
    // "5" is shorter than 2 chars, so the parser skips suffix detection
    // and uses the bare-pixels path.
    assert_eq!(PdfSize::parse("5").unwrap(), PdfSize::Pixels(5.0));
  }

  // ── PdfSize::to_inches conversion constants ──────────────────────────

  #[test]
  fn pixels_convert_using_96_dpi() {
    assert!((PdfSize::Pixels(96.0).to_inches() - 1.0).abs() < 1e-9);
  }

  #[test]
  fn inches_are_identity() {
    assert!((PdfSize::Inches(2.5).to_inches() - 2.5).abs() < 1e-9);
  }

  #[test]
  fn centimeters_convert_using_table_constants() {
    // 37.8 / 96 (exact constant).
    let expected = 10.0 * 37.8 / 96.0;
    assert!((PdfSize::Centimeters(10.0).to_inches() - expected).abs() < 1e-9);
  }

  #[test]
  fn millimeters_convert_using_table_constants() {
    let expected = 25.0 * 3.78 / 96.0;
    assert!((PdfSize::Millimeters(25.0).to_inches() - expected).abs() < 1e-9);
  }

  // ── Paper format lookup ──────────────────────────────────────────────

  #[test]
  fn paper_formats_return_canonical_sizes() {
    assert_eq!(pdf_paper_format_size("Letter"), Some((8.5, 11.0)));
    assert_eq!(pdf_paper_format_size("A4"), Some((8.27, 11.7)));
    assert_eq!(pdf_paper_format_size("tabloid"), Some((11.0, 17.0)));
    assert_eq!(pdf_paper_format_size("LEDGER"), Some((17.0, 11.0)));
  }

  #[test]
  fn unknown_paper_format_returns_none() {
    assert_eq!(pdf_paper_format_size("A99"), None);
    assert_eq!(pdf_paper_format_size(""), None);
  }

  // ── PdfOptions default is fully empty (CDP-defaults-apply) ───────────

  #[test]
  fn default_pdf_options_has_no_overrides() {
    let opts = PdfOptions::default();
    assert!(opts.format.is_none());
    assert!(opts.path.is_none());
    assert!(opts.scale.is_none());
    assert!(opts.display_header_footer.is_none());
    assert!(opts.header_template.is_none());
    assert!(opts.footer_template.is_none());
    assert!(opts.print_background.is_none());
    assert!(opts.landscape.is_none());
    assert!(opts.page_ranges.is_none());
    assert!(opts.width.is_none());
    assert!(opts.height.is_none());
    assert!(opts.margin.is_none());
    assert!(opts.prefer_css_page_size.is_none());
    assert!(opts.outline.is_none());
    assert!(opts.tagged.is_none());
  }
}

#[cfg(test)]
mod media_override_tests {
  use super::*;

  #[test]
  fn default_is_unchanged() {
    let o: MediaOverride = MediaOverride::default();
    assert_eq!(o, MediaOverride::Unchanged);
    assert!(!o.is_specified());
    assert_eq!(o.as_value(), None);
  }

  #[test]
  fn set_reports_value() {
    let o = MediaOverride::Set("dark".into());
    assert!(o.is_specified());
    assert_eq!(o.as_value(), Some("dark"));
  }

  #[test]
  fn disabled_is_specified_without_value() {
    let o = MediaOverride::Disabled;
    assert!(o.is_specified());
    assert_eq!(o.as_value(), None);
  }

  #[test]
  fn from_option_string_maps_some_to_set_and_none_to_unchanged() {
    let set: MediaOverride = Some("dark".to_string()).into();
    assert_eq!(set, MediaOverride::Set("dark".into()));
    let unch: MediaOverride = None.into();
    assert_eq!(unch, MediaOverride::Unchanged);
  }

  #[test]
  fn an_update_layers_over_what_it_leaves_out() {
    let dark = EmulateMediaOptions {
      color_scheme: MediaOverride::Set("dark".into()),
      ..Default::default()
    };
    let print = EmulateMediaOptions {
      media: MediaOverride::Set("print".into()),
      ..Default::default()
    };
    let both = print.layered_over(&dark);
    assert_eq!(both.media, MediaOverride::Set("print".into()));
    assert_eq!(both.color_scheme, MediaOverride::Set("dark".into()));
    let cleared = EmulateMediaOptions {
      color_scheme: MediaOverride::Disabled,
      ..Default::default()
    }
    .layered_over(&both);
    assert_eq!(cleared.color_scheme, MediaOverride::Disabled);
    assert_eq!(cleared.media, MediaOverride::Set("print".into()));
    assert!(EmulateMediaOptions::default().is_empty());
    assert!(!cleared.is_empty());
  }

  #[test]
  fn default_emulate_media_is_all_unchanged() {
    let o = EmulateMediaOptions::default();
    assert_eq!(o.media, MediaOverride::Unchanged);
    assert_eq!(o.color_scheme, MediaOverride::Unchanged);
    assert_eq!(o.reduced_motion, MediaOverride::Unchanged);
    assert_eq!(o.forced_colors, MediaOverride::Unchanged);
    assert_eq!(o.contrast, MediaOverride::Unchanged);
  }
}

#[cfg(test)]
mod drag_option_tests {
  use super::*;

  #[test]
  fn default_drag_options_has_no_overrides() {
    let opts = DragAndDropOptions::default();
    assert!(opts.force.is_none());
    assert!(opts.no_wait_after.is_none());
    assert!(opts.source_position.is_none());
    assert!(opts.target_position.is_none());
    assert!(opts.steps.is_none());
    assert!(opts.strict.is_none());
    assert!(opts.timeout.is_none());
    assert!(opts.trial.is_none());
  }

  #[test]
  fn drag_options_carry_every_field() {
    let opts = DragAndDropOptions {
      force: Some(true),
      no_wait_after: Some(false),
      source_position: Some(Point { x: 10.0, y: 20.0 }),
      target_position: Some(Point { x: 30.0, y: 40.0 }),
      steps: Some(5),
      scroll: None,
      strict: Some(true),
      timeout: Some(2_000),
      trial: Some(true),
    };
    assert_eq!(opts.force, Some(true));
    assert_eq!(opts.no_wait_after, Some(false));
    assert_eq!(opts.source_position, Some(Point { x: 10.0, y: 20.0 }));
    assert_eq!(opts.target_position, Some(Point { x: 30.0, y: 40.0 }));
    assert_eq!(opts.steps, Some(5));
    assert_eq!(opts.strict, Some(true));
    assert_eq!(opts.timeout, Some(2_000));
    assert_eq!(opts.trial, Some(true));
  }

  #[test]
  fn point_default_is_origin() {
    assert_eq!(Point::default(), Point { x: 0.0, y: 0.0 });
  }

  #[test]
  fn default_drop_options_and_payload_are_empty() {
    let opts = DropOptions::default();
    assert_eq!(opts.modifiers, [] as [crate::options::Modifier; 0]);
    assert!(opts.position.is_none());
    assert!(opts.timeout.is_none());

    let payload = DropPayload::default();
    assert!(payload.files.is_none());
    assert_eq!(payload.data, [] as [(String, String); 0]);
  }

  #[test]
  fn drop_payload_carries_files_and_data() {
    let payload = DropPayload {
      files: Some(InputFiles::Payloads(vec![FilePayload {
        name: "card.png".into(),
        mime_type: "image/png".into(),
        buffer: vec![1, 2, 3],
      }])),
      data: vec![("text/plain".into(), "dropped".into())],
    };
    match payload.files {
      Some(InputFiles::Payloads(p)) => {
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "card.png");
        assert_eq!(p[0].mime_type, "image/png");
        assert_eq!(p[0].buffer, vec![1, 2, 3]);
      },
      _ => panic!("expected payloads"),
    }
    assert_eq!(payload.data, vec![("text/plain".to_string(), "dropped".to_string())]);
  }

  #[test]
  fn drop_options_carry_modifiers_position_timeout() {
    let opts = DropOptions {
      modifiers: vec![Modifier::Shift, Modifier::ControlOrMeta],
      position: Some(Point { x: 5.0, y: 7.0 }),
      timeout: Some(1_500),
    };
    assert_eq!(opts.modifiers, vec![Modifier::Shift, Modifier::ControlOrMeta]);
    assert_eq!(opts.position, Some(Point { x: 5.0, y: 7.0 }));
    assert_eq!(opts.timeout, Some(1_500));
  }
}

#[cfg(test)]
mod init_script_tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn function_with_undefined_arg_renders_literal_undefined() {
    // `Object.is(arg, undefined) ? 'undefined' : JSON.stringify(arg)`.
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "(x) => x + 1".to_string(),
      },
      None,
    )
    .unwrap();
    assert_eq!(src, "((x) => x + 1)(undefined)");
  }

  #[test]
  fn function_with_null_arg_renders_literal_null() {
    // `Object.is(null, undefined)` is false — null goes through JSON.stringify.
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "(x) => x".to_string(),
      },
      Some(&serde_json::Value::Null),
    )
    .unwrap();
    assert_eq!(src, "((x) => x)(null)");
  }

  #[test]
  fn function_with_object_arg_renders_json() {
    let arg = json!({ "answer": 42, "nested": [1, 2, 3] });
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "function (o) { window.x = o; }".to_string(),
      },
      Some(&arg),
    )
    .unwrap();
    assert_eq!(
      src,
      r#"(function (o) { window.x = o; })({"answer":42,"nested":[1,2,3]})"#
    );
  }

  #[test]
  fn source_without_arg_passes_through_verbatim() {
    let src = evaluation_script(InitScriptSource::Source("window.x = 1".into()), None).unwrap();
    assert_eq!(src, "window.x = 1");
  }

  #[test]
  fn source_with_arg_errors() {
    let err = evaluation_script(InitScriptSource::Source("window.x = 1".into()), Some(&json!(42))).unwrap_err();
    assert!(
      err.to_string().contains("Cannot evaluate a string with arguments"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn content_with_arg_errors() {
    let err = evaluation_script(InitScriptSource::Content("1".into()), Some(&json!(0))).unwrap_err();
    assert!(err.to_string().contains("Cannot evaluate a string with arguments"));
  }

  #[test]
  fn path_with_arg_errors() {
    let err = evaluation_script(
      InitScriptSource::Path(std::path::PathBuf::from("/nope")),
      Some(&json!(0)),
    )
    .unwrap_err();
    assert!(err.to_string().contains("Cannot evaluate a string with arguments"));
  }

  #[test]
  fn path_reads_file_and_appends_source_url() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("fd-init-script-{}.js", std::process::id()));
    std::fs::write(&path, "window.__fromFile = 7;").unwrap();
    let src = evaluation_script(InitScriptSource::Path(path.clone()), None).unwrap();
    let expected = format!("window.__fromFile = 7;\n//# sourceURL={}", path.display());
    assert_eq!(src, expected);
    let _ = std::fs::remove_file(path);
  }

  #[test]
  fn path_missing_errors() {
    let missing = std::path::PathBuf::from("/definitely/not/a/real/path/x.js");
    let err = evaluation_script(InitScriptSource::Path(missing), None).unwrap_err();
    // Surfaces as Io(_) via FerriError::From<io::Error>.
    assert!(matches!(err, crate::error::FerriError::Io(_)), "unexpected: {err}");
  }

  #[test]
  fn content_passes_through_verbatim() {
    let src = evaluation_script(InitScriptSource::Content("let z = 2;".into()), None).unwrap();
    assert_eq!(src, "let z = 2;");
  }
}

#[cfg(test)]
mod click_option_tests {
  use super::*;

  #[test]
  fn mouse_button_parse_round_trip() {
    assert_eq!(MouseButton::parse("left"), Some(MouseButton::Left));
    assert_eq!(MouseButton::parse("right"), Some(MouseButton::Right));
    assert_eq!(MouseButton::parse("middle"), Some(MouseButton::Middle));
    assert_eq!(MouseButton::parse("garbage"), None);
    assert_eq!(MouseButton::Left.as_cdp(), "left");
    assert_eq!(MouseButton::Right.as_cdp(), "right");
    assert_eq!(MouseButton::Middle.as_cdp(), "middle");
    assert_eq!(MouseButton::Left.as_bidi(), 0);
    assert_eq!(MouseButton::Middle.as_bidi(), 1);
    assert_eq!(MouseButton::Right.as_bidi(), 2);
    // Legacy native-WebKit ordering differs from CDP/BiDi: 0=left,
    // 1=right, 2=middle.
    assert_eq!(MouseButton::Left.as_webkit(), 0);
    assert_eq!(MouseButton::Right.as_webkit(), 1);
    assert_eq!(MouseButton::Middle.as_webkit(), 2);
  }

  #[test]
  fn modifier_parse_and_bits() {
    assert_eq!(Modifier::parse("Alt"), Some(Modifier::Alt));
    assert_eq!(Modifier::parse("Control"), Some(Modifier::Control));
    assert_eq!(Modifier::parse("Meta"), Some(Modifier::Meta));
    assert_eq!(Modifier::parse("Shift"), Some(Modifier::Shift));
    assert_eq!(Modifier::parse("ControlOrMeta"), Some(Modifier::ControlOrMeta));
    assert_eq!(Modifier::parse("garbage"), None);

    assert_eq!(Modifier::Alt.cdp_bit(), 1);
    assert_eq!(Modifier::Control.cdp_bit(), 2);
    assert_eq!(Modifier::Meta.cdp_bit(), 4);
    assert_eq!(Modifier::Shift.cdp_bit(), 8);

    // Platform-aware ControlOrMeta
    if cfg!(target_os = "macos") {
      assert_eq!(Modifier::ControlOrMeta.cdp_bit(), 4);
    } else {
      assert_eq!(Modifier::ControlOrMeta.cdp_bit(), 2);
    }
  }

  #[test]
  fn modifiers_bitmask_folds_multiple() {
    assert_eq!(modifiers_bitmask(&[]), 0);
    assert_eq!(modifiers_bitmask(&[Modifier::Shift]), 8);
    // Alt|Control|Meta|Shift = 1|2|4|8 = 15
    assert_eq!(
      modifiers_bitmask(&[Modifier::Alt, Modifier::Control, Modifier::Meta, Modifier::Shift]),
      15
    );
    // Dedup via bitwise OR — duplicates don't double-count.
    assert_eq!(modifiers_bitmask(&[Modifier::Shift, Modifier::Shift]), 8);
  }

  #[test]
  fn click_options_default_values() {
    let opts = ClickOptions::default();
    assert_eq!(opts.resolved_button(), MouseButton::Left);
    assert_eq!(opts.resolved_click_count(), 1);
    assert_eq!(opts.resolved_delay_ms(), 0);
    assert_eq!(opts.resolved_steps(), 1);
    assert!(!opts.is_force());
    assert!(!opts.is_trial());
    assert_eq!(opts.modifiers, [] as [crate::options::Modifier; 0]);
    assert!(opts.position.is_none());
    assert!(opts.timeout.is_none());
    assert!(opts.no_wait_after.is_none());
  }

  #[test]
  fn click_options_resolved_helpers_use_overrides() {
    let opts = ClickOptions {
      button: Some(MouseButton::Right),
      click_count: Some(2),
      delay: Some(150),
      steps: Some(5),
      force: Some(true),
      trial: Some(true),
      ..Default::default()
    };
    assert_eq!(opts.resolved_button(), MouseButton::Right);
    assert_eq!(opts.resolved_click_count(), 2);
    assert_eq!(opts.resolved_delay_ms(), 150);
    assert_eq!(opts.resolved_steps(), 5);
    assert!(opts.is_force());
    assert!(opts.is_trial());
  }

  #[test]
  fn click_options_steps_coerces_zero_to_one() {
    // defaults to 1 and uses `Math.max(1, steps)`; mirror
    // the clamp so callers passing `0` still emit one mousemove.
    let opts = ClickOptions {
      steps: Some(0),
      ..Default::default()
    };
    assert_eq!(opts.resolved_steps(), 1);
  }
}

#[cfg(test)]
mod storage_state_tests {
  use super::*;

  /// The exported `StorageState` must serialize to Playwright's exact wire
  /// shape: `{ cookies: [...], origins: [{ origin, localStorage: [{name,
  /// value}] }] }` (client/types.ts:42, protocol channels.d.ts:5158/5399).
  #[test]
  fn storage_state_serializes_to_playwright_shape() {
    let state = StorageState {
      cookies: vec![crate::backend::CookieData {
        name: "sid".into(),
        value: "abc".into(),
        domain: "example.com".into(),
        path: "/".into(),
        secure: false,
        http_only: false,
        expires: None,
        same_site: None,
        url: None,
      }],
      origins: vec![OriginState {
        origin: "https://example.com".into(),
        local_storage: vec![NameValue {
          name: "token".into(),
          value: "t1".into(),
        }],
        ..OriginState::default()
      }],
      credentials: None,
    };
    let v = serde_json::to_value(&state).unwrap();
    assert_eq!(v["cookies"][0]["name"], "sid");
    assert_eq!(v["cookies"][0]["value"], "abc");
    // camelCase on the wire — `localStorage`, not `local_storage`.
    assert_eq!(v["origins"][0]["origin"], "https://example.com");
    assert_eq!(v["origins"][0]["localStorage"][0]["name"], "token");
    assert_eq!(v["origins"][0]["localStorage"][0]["value"], "t1");
    assert!(v["origins"][0].get("local_storage").is_none());
  }

  /// A round-trip through the EXPORT shape must be re-consumable as
  /// [`StorageStateInput::Inline`] (same JSON), so saved auth state hydrates.
  #[test]
  fn exported_state_is_valid_storage_state_input() {
    let json = serde_json::json!({
      "cookies": [],
      "origins": [{ "origin": "https://e.com", "localStorage": [{ "name": "k", "value": "v" }] }]
    });
    let parsed: StorageState = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(parsed.origins[0].origin, "https://e.com");
    assert_eq!(parsed.origins[0].local_storage[0].name, "k");
    // The same value is accepted as inline hydration input.
    assert!(matches!(StorageStateInput::Inline(json), StorageStateInput::Inline(_)));
  }
}

#[cfg(test)]
mod proxy_lowering_tests {
  use super::ProxyConfig;
  use crate::backend::BackendKind;

  fn proxy() -> ProxyConfig {
    ProxyConfig {
      server: "http://127.0.0.1:3052".to_string(),
      bypass: Some("127.0.0.1,localhost".to_string()),
      username: None,
      password: None,
    }
  }

  #[test]
  fn chromium_takes_proxy_server() {
    for backend in [BackendKind::CdpPipe, BackendKind::CdpWs] {
      let flags = proxy().launch_flags(backend);
      assert!(
        flags.contains(&"--proxy-server=http://127.0.0.1:3052".to_string()),
        "{backend:?}"
      );
      assert!(
        flags.contains(&"--proxy-bypass-list=127.0.0.1,localhost".to_string()),
        "{backend:?}"
      );
    }
  }

  #[test]
  fn webkit_takes_a_different_switch_entirely() {
    let flags = proxy().launch_flags(BackendKind::WebKit);

    // WebKit accepts `--proxy-server` and ignores it, so the wrong spelling is
    // a proxy that silently does nothing rather than a launch failure.
    assert!(flags.contains(&"--proxy=http://127.0.0.1:3052".to_string()));
    assert!(!flags.iter().any(|flag| flag.starts_with("--proxy-server=")));
  }

  #[test]
  fn webkit_bypass_is_spelled_per_platform() {
    let flags = proxy().launch_flags(BackendKind::WebKit);

    if cfg!(target_os = "linux") {
      assert!(flags.contains(&"--ignore-host=127.0.0.1".to_string()));
      assert!(flags.contains(&"--ignore-host=localhost".to_string()));
    } else {
      assert!(flags.contains(&"--proxy-bypass-list=127.0.0.1,localhost".to_string()));
    }
  }

  #[test]
  fn credentials_are_never_a_launch_flag() {
    let with_credentials = ProxyConfig {
      username: Some("user".to_string()),
      password: Some("secret".to_string()),
      ..proxy()
    };

    // They are answered on the auth challenge; a password on a command line
    // would be readable in every process listing on the machine.
    for flag in with_credentials.launch_flags(BackendKind::CdpPipe) {
      assert!(!flag.contains("secret"), "credential leaked into {flag}");
      assert!(!flag.contains("user"), "credential leaked into {flag}");
    }
  }

  #[test]
  fn a_proxy_without_bypass_lowers_to_one_flag() {
    let no_bypass = ProxyConfig {
      bypass: None,
      ..proxy()
    };

    assert_eq!(no_bypass.launch_flags(BackendKind::WebKit).len(), 1);
    assert_eq!(
      no_bypass.launch_flags(BackendKind::CdpWs),
      [
        "--proxy-server=http://127.0.0.1:3052",
        "--proxy-bypass-list=<-loopback>"
      ]
    );
  }

  #[test]
  fn chromium_sends_loopback_through_the_proxy_unless_the_bypass_names_it() {
    let with = |bypass: Option<&str>| ProxyConfig {
      bypass: bypass.map(ToString::to_string),
      ..proxy()
    };

    assert_eq!(with(None).chromium_bypass_list().as_deref(), Some("<-loopback>"));
    assert_eq!(with(Some(" ")).chromium_bypass_list().as_deref(), Some("<-loopback>"));
    assert_eq!(
      with(Some(".example.com")).chromium_bypass_list().as_deref(),
      Some("<-loopback>,.example.com")
    );
    for loopback in [
      "localhost",
      "127.0.0.1",
      "::1",
      "[::]",
      "[::1]",
      "<loopback>",
      "<-loopback>",
    ] {
      let bypass = format!(".example.com, {loopback}");
      assert!(with(Some(&bypass)).bypasses_loopback(), "{loopback}");
      assert_eq!(
        with(Some(&bypass)).chromium_bypass_list(),
        Some(format!(".example.com,{loopback}")),
        "{loopback}"
      );
    }
    assert!(!with(Some("localhost.example.com,127.0.0.2")).bypasses_loopback());
  }

  #[test]
  fn engines_take_the_server_as_an_origin() {
    let with = |server: &str| ProxyConfig {
      server: server.to_string(),
      ..proxy()
    };
    assert_eq!(with("127.0.0.1:3128").server_origin(), "http://127.0.0.1:3128");
    assert_eq!(
      with(" HTTP://proxy.example.com:3128/ ").server_origin(),
      "http://proxy.example.com:3128"
    );
    assert_eq!(
      with("http://user:pass@proxy.example.com:3128/path?q").server_origin(),
      "http://proxy.example.com:3128"
    );
    assert_eq!(
      with("socks5h://127.0.0.1:1080").chromium_server(),
      "socks5://127.0.0.1:1080"
    );
    assert_eq!(
      with("127.0.0.1:3128").launch_flags(BackendKind::WebKit)[0],
      "--proxy=http://127.0.0.1:3128"
    );
  }

  #[test]
  fn bypass_entries_reach_every_engine_trimmed() {
    let spaced = ProxyConfig {
      bypass: Some(" a.example.com ,localhost, ".to_string()),
      ..proxy()
    };
    assert_eq!(spaced.bypass_list().as_deref(), Some("a.example.com,localhost"));
    assert_eq!(
      spaced.launch_flags(BackendKind::CdpPipe)[1..],
      ["--proxy-bypass-list=a.example.com,localhost"]
    );
    let webkit = spaced.launch_flags(BackendKind::WebKit);
    if cfg!(target_os = "linux") {
      assert_eq!(webkit[1..], ["--ignore-host=a.example.com", "--ignore-host=localhost"]);
    } else {
      assert_eq!(webkit[1..], ["--proxy-bypass-list=a.example.com,localhost"]);
    }
    let blank = ProxyConfig {
      bypass: Some(" , ".to_string()),
      ..proxy()
    };
    assert_eq!(blank.bypass_list(), None);
    assert_eq!(
      blank.launch_flags(BackendKind::CdpPipe)[1..],
      ["--proxy-bypass-list=<-loopback>"]
    );
  }
}

#[cfg(test)]
mod browser_selection_tests {
  use super::{BrowserKind, BrowserSelection};
  use crate::backend::BackendKind;

  #[test]
  fn products_select_their_driver_without_transport_configuration() {
    for (browser, backend) in [
      (BrowserKind::Chromium, BackendKind::CdpPipe),
      (BrowserKind::Firefox, BackendKind::Bidi),
      (BrowserKind::WebKit, BackendKind::WebKit),
      (BrowserKind::Safari, BackendKind::WebDriver),
      (BrowserKind::Ladybird, BackendKind::Bidi),
      (BrowserKind::Lightpanda, BackendKind::CdpWs),
    ] {
      assert_eq!(
        BrowserSelection::resolve(Some(browser), None).unwrap(),
        BrowserSelection { browser, backend }
      );
    }
  }

  #[test]
  fn explicit_protocols_never_change_the_requested_product() {
    for browser in [
      BrowserKind::Chromium,
      BrowserKind::Firefox,
      BrowserKind::WebKit,
      BrowserKind::Safari,
      BrowserKind::Ladybird,
      BrowserKind::Lightpanda,
    ] {
      for backend in [
        BackendKind::CdpPipe,
        BackendKind::CdpWs,
        BackendKind::Bidi,
        BackendKind::WebKit,
        BackendKind::WebDriver,
      ] {
        let expected = match browser {
          BrowserKind::Chromium => backend != BackendKind::WebKit,
          BrowserKind::Firefox | BrowserKind::Safari | BrowserKind::Ladybird => {
            matches!(backend, BackendKind::Bidi | BackendKind::WebDriver)
          },
          BrowserKind::WebKit => backend == BackendKind::WebKit,
          BrowserKind::Lightpanda => backend == BackendKind::CdpWs,
        };
        let result = BrowserSelection::resolve(Some(browser), Some(backend));
        assert_eq!(result.is_ok(), expected, "{browser:?} with {backend:?}");
        if let Ok(selection) = result {
          assert_eq!(selection.browser, browser);
        }
      }
    }
  }

  #[test]
  fn protocol_and_transport_resolve_orthogonally() {
    use super::ChromiumTransport;
    use crate::backend::Protocol;

    assert_eq!(BackendKind::parse("cdp").unwrap(), BackendKind::CdpPipe);
    assert_eq!(BackendKind::CdpPipe.protocol(), Protocol::Cdp);
    assert_eq!(BackendKind::CdpWs.protocol(), Protocol::Cdp);
    assert_eq!(BackendKind::CdpPipe.transport(), Some(ChromiumTransport::Pipe));
    assert_eq!(BackendKind::CdpWs.transport(), Some(ChromiumTransport::Ws));
    assert_eq!(BackendKind::Bidi.transport(), None);

    assert_eq!(
      BrowserSelection::resolve_with_transport(
        Some(BrowserKind::Chromium),
        Some(BackendKind::parse("cdp").unwrap()),
        Some(ChromiumTransport::Ws),
      )
      .unwrap(),
      BrowserSelection {
        browser: BrowserKind::Chromium,
        backend: BackendKind::CdpWs,
      }
    );
    assert_eq!(
      BrowserSelection::resolve_with_transport(
        Some(BrowserKind::Lightpanda),
        Some(BackendKind::parse("cdp").unwrap()),
        Some(ChromiumTransport::Ws),
      )
      .unwrap(),
      BrowserSelection {
        browser: BrowserKind::Lightpanda,
        backend: BackendKind::CdpWs,
      }
    );
    assert!(
      BrowserSelection::resolve_with_transport(
        Some(BrowserKind::Firefox),
        Some(BackendKind::Bidi),
        Some(ChromiumTransport::Ws),
      )
      .is_err()
    );
    assert!(
      BrowserSelection::resolve_with_transport(
        Some(BrowserKind::Lightpanda),
        Some(BackendKind::parse("cdp").unwrap()),
        Some(ChromiumTransport::Pipe),
      )
      .is_err()
    );
  }
}
