//! Configuration unit tests.

use super::*;

#[cfg(test)]
mod color_scheme_config_tests {
  use super::{ColorSchemeSetting, ContextConfig};

  fn parse(json: &str) -> ContextConfig {
    serde_json::from_str(json).expect("parse")
  }

  #[test]
  fn null_is_kept_apart_from_an_absent_key() {
    assert_eq!(parse("{}").color_scheme, None);
    assert_eq!(
      parse(r#"{"colorScheme":null}"#).color_scheme,
      Some(ColorSchemeSetting::Null)
    );
    assert_eq!(
      parse(r#"{"colorScheme":"dark"}"#).color_scheme,
      Some(ColorSchemeSetting::Scheme("dark".into()))
    );
  }

  #[test]
  fn an_absent_key_survives_a_round_trip() {
    let written = serde_json::to_string(&parse("{}")).expect("serialize");
    assert_eq!(parse(&written).color_scheme, None);
    let written = serde_json::to_string(&parse(r#"{"colorScheme":null}"#)).expect("serialize");
    assert_eq!(parse(&written).color_scheme, Some(ColorSchemeSetting::Null));
  }

  #[test]
  fn the_fixture_value_is_light_unless_written() {
    assert_eq!(parse("{}").color_scheme_fixture(), Some("light"));
    assert_eq!(parse(r#"{"colorScheme":"dark"}"#).color_scheme_fixture(), Some("dark"));
    assert_eq!(parse(r#"{"colorScheme":null}"#).color_scheme_fixture(), None);
  }
}

#[cfg(test)]
mod reporter_config_tests {
  use std::collections::BTreeMap;

  use super::ReporterConfig;

  fn parse(toml_src: &str) -> Vec<ReporterConfig> {
    #[derive(serde::Deserialize)]
    struct Wrapper {
      reporter: Vec<ReporterConfig>,
    }
    toml::from_str::<Wrapper>(toml_src).expect("parse").reporter
  }

  #[test]
  fn options_beside_the_name_are_options() {
    let parsed = parse(
      r#"
        [[reporter]]
        name = "junit"
        outputFile = "j.xml"
        includeRetries = true
      "#,
    );
    assert_eq!(parsed[0].name, "junit");
    assert_eq!(parsed[0].options["outputFile"], serde_json::json!("j.xml"));
    assert_eq!(parsed[0].options["includeRetries"], serde_json::json!(true));
  }

  #[test]
  fn an_explicit_options_table_still_works_and_wins() {
    let parsed = parse(
      r#"
        [[reporter]]
        name = "junit"
        outputFile = "beside.xml"
        [reporter.options]
        outputFile = "table.xml"
      "#,
    );
    assert_eq!(parsed[0].options["outputFile"], serde_json::json!("table.xml"));
  }

  #[test]
  fn a_bare_string_names_a_reporter_without_options() {
    let parsed = parse(r#"reporter = ["list", "html"]"#);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name, "list");
    assert!(parsed[1].options.is_empty());
  }

  #[test]
  fn playwright_pairs_name_a_reporter_and_its_options() {
    let parsed = parse(r#"reporter = [["list"], ["junit", { outputFile = "j.xml" }]]"#);
    assert_eq!(parsed[0].name, "list");
    assert!(parsed[0].options.is_empty());
    assert_eq!(parsed[1].name, "junit");
    assert_eq!(parsed[1].options["outputFile"], serde_json::json!("j.xml"));
  }

  #[test]
  fn a_pair_without_a_name_or_with_a_third_entry_is_an_error() {
    #[derive(serde::Deserialize)]
    struct Wrapper {
      reporter: Vec<ReporterConfig>,
    }
    let parse = |src: &str| toml::from_str::<Wrapper>(src).map(|wrapper| wrapper.reporter);
    assert!(parse("reporter = [[{ outputFile = \"x\" }]]\n").is_err());
    assert!(parse("reporter = [[\"list\", {}, {}]]\n").is_err());
  }

  #[test]
  fn a_test_section_takes_playwright_single_values() {
    let config: super::TestConfig = serde_json::from_value(serde_json::json!({
      "reporter": "html",
      "testMatch": "e2e/**/*.spec.ts",
      "testIgnore": "e2e/console/**",
      "projects": [{ "name": "console", "testMatch": "console/*.spec.ts", "testIgnore": ["a/**", "b/**"] }],
    }))
    .expect("parse");
    assert_eq!(config.reporter.len(), 1);
    assert_eq!(config.reporter[0].name, "html");
    assert_eq!(config.test_match, ["e2e/**/*.spec.ts"]);
    assert_eq!(config.test_ignore, ["e2e/console/**"]);
    assert_eq!(
      config.projects[0].test_match.as_deref(),
      Some(&["console/*.spec.ts".to_string()][..])
    );
    assert_eq!(config.projects[0].test_ignore.as_deref().map(<[String]>::len), Some(2));
  }

  #[test]
  fn a_missing_name_is_an_error_rather_than_a_nameless_reporter() {
    let parsed = toml::from_str::<BTreeMap<String, Vec<ReporterConfig>>>("[[reporter]]\noutputFile = \"x\"\n");
    assert!(parsed.is_err());
  }
}

#[cfg(test)]
mod expect_config_tests {
  use super::{ExpectConfig, ProjectConfig, TestConfig};

  fn config(toml_src: &str) -> TestConfig {
    toml::from_str::<TestConfig>(toml_src).expect("parse")
  }

  fn config_error(toml_src: &str) -> String {
    toml::from_str::<TestConfig>(toml_src)
      .expect_err("rejected")
      .to_string()
  }

  #[test]
  fn video_takes_a_mode_or_a_mode_with_its_size() {
    let on = config("video = \"on\"\n").video;
    assert_eq!((on.mode, on.width, on.height), (super::VideoMode::On, None, None));
    let sized = config("[video]\nmode = \"retain-on-failure\"\nwidth = 640\nheight = 360\n").video;
    assert_eq!(
      (sized.mode, sized.frame_size(Some((1280, 720)))),
      (super::VideoMode::RetainOnFailure, (640, 360))
    );
    assert_eq!(config("").video.mode, super::VideoMode::Off);
  }

  #[test]
  fn an_unsized_video_follows_the_viewport_within_800_by_800() {
    let video = config("video = \"on\"\n").video;
    assert_eq!(video.frame_size(Some((1280, 720))), (800, 450));
    assert_eq!(video.frame_size(Some((393, 852))), (368, 800));
    assert_eq!(video.frame_size(Some((640, 481))), (640, 480));
    assert_eq!(video.frame_size(None), (800, 600));
  }

  #[test]
  fn workers_take_a_count_a_numeric_string_or_a_percentage() {
    let cpus =
      u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)).expect("cpus");
    assert_eq!(config("workers = 3\n").workers, Some(3));
    assert_eq!(config("workers = \"2\"\n").workers, Some(2));
    assert_eq!(config("workers = \"100%\"\n").workers, Some(cpus));
    assert_eq!(config("workers = \"1%\"\n").workers, Some((cpus / 100).max(1)));
    assert_eq!(super::parse_workers("4x"), Ok(4), "parseInt reads the leading digits");
  }

  #[test]
  fn workers_below_one_or_unreadable_are_refused_with_playwrights_message() {
    assert!(config_error("workers = 0\n").contains("Workers must be a positive number, received 0."));
    assert!(config_error("workers = \"-2\"\n").contains("Workers must be a positive number, received -2."));
    assert!(config_error("workers = \"0%\"\n").contains("Workers must be a positive number, received 0."));
    assert!(config_error("workers = \"many\"\n").contains("Workers many must be a number or percentage."));
    assert!(config_error("workers = 1.5\n").contains("Workers must be a whole number, received 1.5."));
  }

  #[test]
  fn a_project_caps_its_workers_inside_the_runs_budget() {
    let cfg = config("workers = 4\n[[projects]]\nname = \"a\"\nworkers = 2\n[[projects]]\nname = \"b\"\nworkers = 8\n");
    assert_eq!(cfg.merge_project(&cfg.projects[0]).workers, Some(2));
    assert_eq!(cfg.merge_project(&cfg.projects[1]).workers, Some(4));
    assert!(config_error("[[projects]]\nname = \"a\"\nworkers = 0\n").contains("Workers must be a positive number"));
  }

  #[test]
  fn the_flat_expect_timeout_is_the_nested_ones_older_spelling() {
    let cfg = config("expectTimeout = 900\n");
    assert_eq!(cfg.resolved_expect(None).timeout_ms(), 900);

    // The nested key wins where both are set.
    let cfg = config("expectTimeout = 900\n[expect]\ntimeout = 1200\n");
    assert_eq!(cfg.resolved_expect(None).timeout_ms(), 1200);
  }

  #[test]
  fn a_project_expect_block_replaces_the_config_one_whole() {
    // The config sets a screenshot budget AND a timeout; the project
    // sets ONLY a timeout. Playwright's `takeFirst` hands back the
    // project's object as-is, so the screenshot budget is NOT inherited
    // (`playwright/src/common/config.ts:201`).
    let cfg = config(
      r"
        [expect]
        timeout = 1200
        [expect.toHaveScreenshot]
        maxDiffPixelRatio = 0.4
      ",
    );
    let project = ProjectConfig {
      name: "narrow".to_string(),
      expect: Some(ExpectConfig {
        timeout: Some(900),
        ..ExpectConfig::default()
      }),
      ..ProjectConfig::default()
    };

    let resolved = cfg.resolved_expect(Some(&project));
    assert_eq!(resolved.timeout_ms(), 900);
    assert_eq!(
      resolved.to_have_screenshot.max_diff_pixel_ratio, None,
      "a project's expect block replaces the config's whole object, never merges into it"
    );
    // A project with no block of its own does inherit everything.
    let plain = ProjectConfig {
      name: "plain".to_string(),
      ..ProjectConfig::default()
    };
    let inherited = cfg.resolved_expect(Some(&plain));
    assert_eq!(inherited.timeout_ms(), 1200);
    assert_eq!(inherited.to_have_screenshot.max_diff_pixel_ratio, Some(0.4));
  }

  #[test]
  fn a_project_block_without_a_timeout_falls_back_to_the_default_not_the_config() {
    let cfg = config("expectTimeout = 900\n[expect]\ntimeout = 1200\n");
    let project = ProjectConfig {
      name: "p".to_string(),
      expect: Some(ExpectConfig {
        to_pass: super::ToPassConfig {
          timeout: Some(50),
          intervals: None,
        },
        ..ExpectConfig::default()
      }),
      ..ProjectConfig::default()
    };
    let resolved = cfg.resolved_expect(Some(&project));
    assert_eq!(resolved.to_pass.timeout, Some(50));
    assert_eq!(
      resolved.timeout_ms(),
      super::DEFAULT_EXPECT_TIMEOUT_MS,
      "the replaced object is gone, so an unset timeout is Playwright's default"
    );
  }

  #[test]
  fn merge_project_settles_both_spellings() {
    let cfg = config("expectTimeout = 900\n");
    let project = ProjectConfig {
      name: "p".to_string(),
      expect: Some(ExpectConfig {
        timeout: Some(300),
        ..ExpectConfig::default()
      }),
      ..ProjectConfig::default()
    };
    let merged = cfg.merge_project(&project);
    assert_eq!(merged.expect.timeout, Some(300));
    assert_eq!(merged.expect_timeout, 300, "the flat key follows the resolved block");
  }

  #[test]
  fn the_style_path_takes_a_string_or_a_list() {
    let cfg = config(
      r#"
        [expect.toHaveScreenshot]
        stylePath = "a.css"
      "#,
    );
    assert_eq!(
      cfg
        .expect
        .to_have_screenshot
        .style_path
        .as_ref()
        .map(super::StringOrList::to_vec),
      Some(vec!["a.css".to_string()])
    );
    let cfg = config(
      r#"
        [expect.toHaveScreenshot]
        stylePath = ["a.css", "b.css"]
      "#,
    );
    assert_eq!(
      cfg
        .expect
        .to_have_screenshot
        .style_path
        .as_ref()
        .map(super::StringOrList::to_vec),
      Some(vec!["a.css".to_string(), "b.css".to_string()])
    );
  }
}

#[cfg(test)]
mod use_options_tests {
  #[test]
  fn browser_viewport_preserves_absence_null_and_size() {
    let absent: super::BrowserConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(absent.viewport.is_none());
    let round_trip: super::BrowserConfig = serde_json::from_value(serde_json::to_value(absent).unwrap()).unwrap();
    assert!(round_trip.viewport.is_none());
    let disabled: super::BrowserConfig = serde_json::from_value(serde_json::json!({"viewport":null})).unwrap();
    assert!(matches!(
      disabled.viewport,
      Some(crate::browser::ViewportOverride::Disabled)
    ));
    let sized: super::BrowserConfig =
      serde_json::from_value(serde_json::json!({"viewport":{"width":412,"height":823}})).unwrap();
    let Some(crate::browser::ViewportOverride::Size(size)) = sized.viewport else {
      panic!("expected explicit viewport")
    };
    assert_eq!((size.width, size.height), (412, 823));
  }

  use super::{ProjectConfig, TestConfig};

  fn config(toml_src: &str) -> TestConfig {
    toml::from_str::<TestConfig>(toml_src).expect("parse")
  }

  #[test]
  fn http_credentials_take_one_table_or_a_list_of_them() {
    let one = config(
      r#"
        [browser.use]
        httpCredentials = { username = "user", password = "pass" }
      "#,
    );
    let one = &one.browser.use_options.http_credentials;
    assert_eq!(one.len(), 1);
    assert_eq!((one[0].username.as_str(), one[0].origin.as_deref()), ("user", None));

    let many = config(
      r#"
        [[browser.use.httpCredentials]]
        username = "alice"
        password = "secret"
        origin = "https://example.com"

        [[browser.use.httpCredentials]]
        username = "user"
        password = "pass"
      "#,
    );
    let many = &many.browser.use_options.http_credentials;
    assert_eq!(many.len(), 2);
    assert_eq!(many[0].origin.as_deref(), Some("https://example.com"));
    assert_eq!(many[1].username, "user");

    assert!(config("").browser.use_options.http_credentials.is_empty());
  }

  #[test]
  fn a_use_key_no_field_claims_survives_parsing() {
    let cfg = config(
      r#"
        [browser.use]
        locale = "de-DE"
        profile = "guest"
        settings = { depth = 2, tags = ["a"] }
      "#,
    );
    assert_eq!(cfg.browser.use_options.locale.as_deref(), Some("de-DE"));
    assert_eq!(cfg.browser.use_options.extra["profile"], serde_json::json!("guest"));
    assert_eq!(
      cfg.browser.use_options.extra["settings"],
      serde_json::json!({ "depth": 2, "tags": ["a"] })
    );
    // A claimed key is NOT duplicated into the open map.
    assert!(!cfg.browser.use_options.extra.contains_key("locale"));
  }

  #[test]
  fn a_project_overlays_open_keys_one_at_a_time() {
    let cfg = config(
      r#"
        [browser.use]
        profile = "guest"
        theme = "light"

        [[projects]]
        name = "admin"
        [projects.browser.use]
        profile = "admin"
      "#,
    );
    let merged = cfg.merge_project(&cfg.projects[0]);
    assert_eq!(merged.browser.use_options.extra["profile"], serde_json::json!("admin"));
    // The key the project did not name is still the config's.
    assert_eq!(merged.browser.use_options.extra["theme"], serde_json::json!("light"));
  }

  /// The root config is normalized before projects merge from it; an
  /// engine normalization filled in must still give way to the engine a
  /// project's device names, as Playwright's `browserName` defaults from
  /// `defaultBrowserType`. One the root names itself still wins.
  #[test]
  fn a_project_device_picks_the_engine_over_a_defaulted_root() {
    let mut cfg = config(
      r#"
        [[projects]]
        name = "firefox"
        [projects.browser.use]
        defaultBrowserType = "firefox"

        [[projects]]
        name = "webkit"
        [projects.browser.use]
        defaultBrowserType = "webkit"
      "#,
    );
    cfg.browser.apply_use_engine();
    cfg.browser.normalize();
    let firefox = cfg.merge_project(&cfg.projects[0]);
    assert_eq!(
      (firefox.browser.browser.as_str(), firefox.browser.backend.as_str()),
      ("firefox", "bidi")
    );
    let webkit = cfg.merge_project(&cfg.projects[1]);
    assert_eq!(
      (webkit.browser.browser.as_str(), webkit.browser.backend.as_str()),
      ("webkit", "webkit")
    );

    let mut named = config(
      r#"
        [browser]
        browser = "chromium"

        [[projects]]
        name = "firefox-device"
        [projects.browser.use]
        defaultBrowserType = "firefox"
      "#,
    );
    named.browser.apply_use_engine();
    named.browser.normalize();
    assert_eq!(named.merge_project(&named.projects[0]).browser.browser, "chromium");
  }

  #[test]
  fn a_project_test_id_attribute_reaches_the_merged_config() {
    let cfg = config(
      r#"
        [browser.use]
        testIdAttribute = "data-testid"

        [[projects]]
        name = "legacy"
        [projects.browser.use]
        testIdAttribute = "data-qa"
      "#,
    );
    let merged = cfg.merge_project(&cfg.projects[0]);
    assert_eq!(merged.browser.use_options.test_id_attribute.as_deref(), Some("data-qa"));
  }

  #[test]
  fn open_use_keys_covers_every_project() {
    let cfg = config(
      r#"
        [browser.use]
        profile = "guest"

        [[projects]]
        name = "one"
        [projects.browser.use]
        profile = "admin"
        onlyHere = 1

        [[projects]]
        name = "two"
      "#,
    );
    let keys: Vec<&str> = cfg.open_use_keys().into_iter().map(String::as_str).collect();
    assert_eq!(keys, vec!["onlyHere", "profile"]);

    // A project with no browser section contributes nothing.
    let bare = TestConfig {
      projects: vec![ProjectConfig::default()],
      ..TestConfig::default()
    };
    assert_eq!(bare.open_use_keys(), [] as [&String; 0]);
  }
}

#[cfg(test)]
mod location_tests {
  use super::TestLocation;

  #[test]
  fn a_trailing_line_and_column_are_a_location() {
    assert_eq!(
      TestLocation::parse("tests/cart.spec.ts:12"),
      Some(TestLocation {
        file: "tests/cart.spec.ts".into(),
        line: 12,
        column: None,
      })
    );
    assert_eq!(
      TestLocation::parse("tests/cart.spec.ts:12:5"),
      Some(TestLocation {
        file: "tests/cart.spec.ts".into(),
        line: 12,
        column: Some(5),
      })
    );
  }

  #[test]
  fn a_plain_path_or_glob_is_not() {
    assert_eq!(TestLocation::parse("tests/**/*.spec.ts"), None);
    assert_eq!(TestLocation::parse("tests/a:b.spec.ts"), None);
    assert_eq!(TestLocation::parse(":12"), None);
  }
}

#[cfg(test)]
mod firefox_user_prefs_tests {
  use super::{ProjectConfig, TestConfig};

  #[test]
  fn a_project_adds_to_the_suite_preferences_and_wins_per_name() {
    let suite: TestConfig = serde_json::from_str(
      r#"{"browser":{"firefoxUserPrefs":{"layout.frame_rate":30,"dom.webnotifications.enabled":false}}}"#,
    )
    .expect("suite");
    let project: ProjectConfig = serde_json::from_str(
      r#"{"name":"bidi","browser":{"browser":"firefox","backend":"bidi","firefoxUserPrefs":{"layout.frame_rate":60}}}"#,
    )
    .expect("project");
    let prefs = suite.merge_project(&project).browser.firefox_user_prefs;
    assert_eq!(prefs.get("layout.frame_rate"), Some(&serde_json::json!(60)));
    assert_eq!(
      prefs.get("dom.webnotifications.enabled"),
      Some(&serde_json::json!(false))
    );
  }
}
