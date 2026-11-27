//! Unit tests for page-quality audits.

use super::*;
use crate::best_practices::{elide_data_uri, guess_mime_type, has_density_descriptor};
use crate::seo::{
  BOT_USER_AGENTS, anchor_is_uncrawlable, header_directive_user_agent, is_javascript_void, is_unavailable,
  link_text_is_non_descriptive, parse_directive_date,
};

#[test]
fn javascript_void_after_a_multibyte_char() {
  assert!(is_javascript_void("\u{e9}javascript:void(0)"));
  assert!(is_javascript_void("javascript:void0"));
  assert!(!is_javascript_void("\u{e9}javascript:voida"));
}

#[test]
fn elide_data_uri_cuts_on_a_char_boundary() {
  let url = format!("data:text/plain,{}", "\u{e9}".repeat(120));
  let elided = elide_data_uri(&url);
  assert_eq!(elided.chars().count(), 100);
  assert!(elided.ends_with('\u{2026}'));
  assert_eq!(elide_data_uri("data:,short"), "data:,short");
}

fn meta(name: &str, content: &str) -> Meta {
  Meta {
    name: name.into(),
    content: content.into(),
    selector: format!("meta[name={name}]"),
    snippet: format!("<meta name=\"{name}\" content=\"{content}\">"),
  }
}

fn header(name: &str, value: &str) -> ferridriver_types::HeaderEntry {
  ferridriver_types::HeaderEntry {
    name: name.into(),
    value: value.into(),
  }
}

/// Artifacts for a page that arrived over HTTP with `status` and
/// `headers`, and nothing else worth auditing.
fn served(status: i64, headers: Vec<ferridriver_types::HeaderEntry>) -> Artifacts {
  Artifacts {
    url: "https://example.com/page".into(),
    main_document: Some(MainDocument {
      url: "https://example.com/page".into(),
      status_code: status,
      response_headers: headers,
    }),
    ..Artifacts::default()
  }
}

fn anchor(raw_href: &str, attribute_names: &[&str]) -> Anchor {
  Anchor {
    href: String::new(),
    raw_href: raw_href.into(),
    name: String::new(),
    role: String::new(),
    id: String::new(),
    rel: String::new(),
    text: String::new(),
    text_lang: None,
    attribute_names: attribute_names.iter().map(|a| (*a).to_string()).collect(),
    has_inline_handler: false,
    selector: "a".into(),
    snippet: "<a>".into(),
  }
}

#[test]
fn a_missing_doctype_and_a_quirks_doctype_are_different_failures() {
  let none = doctype(&Artifacts::default());
  assert!(!none.passed);
  assert_eq!(none.explanation.as_deref(), Some("Document must contain a doctype"));

  let quirks = doctype(&Artifacts {
    doctype: Some(Doctype {
      name: "html".into(),
      public_id: "-//W3C//DTD HTML 4.01//EN".into(),
      system_id: String::new(),
      document_compat_mode: "BackCompat".into(),
    }),
    ..Artifacts::default()
  });
  assert!(!quirks.passed);
  assert_eq!(
    quirks.explanation.as_deref(),
    Some("Expected publicId to be an empty string")
  );
}

#[test]
fn standards_mode_passes_whatever_the_doctype_says() {
  // Upstream returns on compatMode before it looks at any of the
  // three fields, so a peculiar-but-standards doctype passes.
  let report = doctype(&Artifacts {
    doctype: Some(Doctype {
      name: "HTML".into(),
      public_id: String::new(),
      system_id: String::new(),
      document_compat_mode: "CSS1Compat".into(),
    }),
    ..Artifacts::default()
  });
  assert!(report.passed);
}

#[test]
fn a_whitespace_only_meta_description_fails_with_its_own_explanation() {
  let report = meta_description(&Artifacts {
    metas: vec![meta("description", "   ")],
    ..Artifacts::default()
  });
  assert!(!report.passed);
  assert_eq!(report.explanation.as_deref(), Some("Description text is empty."));
}

#[test]
fn a_missing_meta_description_fails_without_one() {
  let report = meta_description(&Artifacts::default());
  assert!(!report.passed);
  assert_eq!(report.explanation, None);
}

#[test]
fn javascript_void_is_recognised_in_every_form_upstream_accepts() {
  for href in [
    "javascript:void0",
    "javascript:void(0",
    "javascript:void0)",
    "javascript:void(0)",
  ] {
    assert!(is_javascript_void(href), "{href}");
  }
  for href in ["javascript:void", "javascript:0", "/void(0)"] {
    assert!(!is_javascript_void(href), "{href}");
  }
}

#[test]
fn an_anchor_with_no_href_fails_only_when_something_handles_it() {
  let bare = anchor("", &[]);
  assert!(!anchor_is_uncrawlable(&bare, "https://example.com/"));

  let mut handled = anchor("", &[]);
  handled.has_inline_handler = true;
  assert!(anchor_is_uncrawlable(&handled, "https://example.com/"));

  // An href-associated attribute takes it out of that branch, and
  // then an empty resolved href fails it outright.
  let mut with_target = anchor("", &["target"]);
  with_target.has_inline_handler = false;
  assert!(anchor_is_uncrawlable(&with_target, "https://example.com/"));
}

#[test]
fn mailto_and_named_anchors_are_left_alone() {
  assert!(!anchor_is_uncrawlable(
    &anchor("mailto:me@example.com", &["href"]),
    "https://example.com/"
  ));
  let mut named = anchor("javascript:void(0)", &["href", "name"]);
  named.name = "top".into();
  assert!(!anchor_is_uncrawlable(&named, "https://example.com/"));
  let mut with_id = anchor("", &["id"]);
  with_id.id = "section".into();
  assert!(!anchor_is_uncrawlable(&with_id, "https://example.com/"));
}

#[test]
fn link_text_matches_any_language_when_the_page_does_not_say_which() {
  let mut link = anchor("/next", &["href"]);
  link.href = "https://example.com/next".into();
  link.text = "  Click Here  ".into();
  assert!(link_text_is_non_descriptive(&link, "https://example.com/"));

  // Told it is German, the English table no longer applies.
  link.text_lang = Some("de-DE".into());
  assert!(!link_text_is_non_descriptive(&link, "https://example.com/"));
  link.text = "hier klicken".into();
  assert!(link_text_is_non_descriptive(&link, "https://example.com/"));
}

#[test]
fn a_link_to_the_page_it_sits_on_is_not_judged_on_its_text() {
  let mut link = anchor("#section", &["href"]);
  link.href = "https://example.com/page#section".into();
  link.text = "here".into();
  assert!(!link_text_is_non_descriptive(&link, "https://example.com/page"));
}

#[test]
fn nofollow_and_javascript_links_are_not_judged_on_their_text() {
  let mut nofollow = anchor("/next", &["href", "rel"]);
  nofollow.href = "https://example.com/next".into();
  nofollow.text = "here".into();
  nofollow.rel = "nofollow".into();
  assert!(!link_text_is_non_descriptive(&nofollow, "https://example.com/"));

  let mut scripted = anchor("javascript:go()", &["href"]);
  scripted.href = "javascript:go()".into();
  scripted.text = "here".into();
  assert!(!link_text_is_non_descriptive(&scripted, "https://example.com/"));
}

#[test]
fn a_stretched_image_fails_and_a_letterboxed_one_does_not() {
  let image = |displayed: (f64, f64), natural: (f64, f64)| Image {
    src: "https://example.com/a.png".into(),
    srcset: String::new(),
    displayed_width: displayed.0,
    displayed_height: displayed.1,
    client_rect: Rect::default(),
    natural_dimensions: Some(Dimensions {
      width: natural.0,
      height: natural.1,
    }),
    object_fit: "fill".into(),
    rendering: "auto".into(),
  };
  let of = |image: Image| {
    image_aspect_ratio(&Artifacts {
      images: vec![image],
      ..Artifacts::default()
    })
    .passed
  };
  assert!(!of(image((200.0, 200.0), (400.0, 100.0))), "square box, wide image");
  assert!(of(image((200.0, 50.0), (400.0, 100.0))), "same ratio, half the size");
  // object-fit takes the image out of the audit entirely.
  let mut contained = image((200.0, 200.0), (400.0, 100.0));
  contained.object_fit = "contain".into();
  assert!(of(contained));
}

#[test]
fn an_image_smaller_than_it_is_drawn_fails_once_it_is_on_screen() {
  let artifacts = |rect: Rect| Artifacts {
    images: vec![Image {
      src: "https://example.com/a.png".into(),
      srcset: String::new(),
      displayed_width: 200.0,
      displayed_height: 200.0,
      client_rect: rect,
      natural_dimensions: Some(Dimensions {
        width: 100.0,
        height: 100.0,
      }),
      object_fit: "fill".into(),
      rendering: "auto".into(),
    }],
    viewport: Viewport {
      inner_width: 800.0,
      inner_height: 600.0,
      device_pixel_ratio: 1.0,
    },
    ..Artifacts::default()
  };
  let on_screen = Rect {
    top: 0.0,
    bottom: 200.0,
    left: 0.0,
    right: 200.0,
  };
  assert!(!image_size_responsive(&artifacts(on_screen)).passed);

  // Scrolled far below the fold, upstream stops caring.
  let off_screen = Rect {
    top: 2000.0,
    bottom: 2200.0,
    left: 0.0,
    right: 200.0,
  };
  assert!(image_size_responsive(&artifacts(off_screen)).passed);
}

#[test]
fn a_density_descriptor_takes_an_image_out_of_the_resolution_audit() {
  assert!(has_density_descriptor("/a.png 2x, /b.png 3x"));
  assert!(has_density_descriptor("/a.png 1.5x"));
  assert!(!has_density_descriptor("/a.png 640w"));
  assert!(!has_density_descriptor("/a.png"));
}

#[test]
fn a_read_only_input_is_never_a_paste_failure() {
  let input = |prevents_paste| Input {
    r#type: "text".into(),
    prevents_paste,
    selector: "input".into(),
    snippet: "<input>".into(),
  };
  let of = |input| {
    paste_preventing_inputs(&Artifacts {
      inputs: vec![input],
      ..Artifacts::default()
    })
    .passed
  };
  assert!(!of(input(Some(true))));
  assert!(of(input(Some(false))));
  assert!(of(input(None)), "read-only, so upstream never dispatched the event");
}

fn link(rel: &str, href_raw: &str, href: &str) -> Link {
  Link {
    rel: rel.into(),
    href_raw: href_raw.into(),
    href: href.into(),
    hreflang: String::new(),
  }
}

#[test]
fn a_relative_canonical_and_an_absolute_one_are_different_verdicts() {
  let of = |links: Vec<Link>, url: &str| {
    let report = canonical(&Artifacts {
      links,
      url: url.into(),
      ..Artifacts::default()
    });
    (report.passed, report.explanation)
  };

  let (passed, _) = of(
    vec![link(
      "canonical",
      "https://example.com/page",
      "https://example.com/page",
    )],
    "https://example.com/page",
  );
  assert!(passed);

  // Resolvable only against a base: upstream calls that relative.
  let (passed, why) = of(
    vec![link("canonical", "/page", "https://example.com/page")],
    "https://example.com/page",
  );
  assert!(!passed);
  assert!(why.unwrap_or_default().contains("Relative"));
}

#[test]
fn two_canonicals_conflict_and_one_repeated_does_not() {
  let two = canonical(&Artifacts {
    links: vec![
      link("canonical", "https://example.com/a", "https://example.com/a"),
      link("canonical", "https://example.com/b", "https://example.com/b"),
    ],
    url: "https://example.com/a".into(),
    ..Artifacts::default()
  });
  assert!(!two.passed);
  assert!(two.explanation.unwrap_or_default().contains("conflicting"));

  let repeated = canonical(&Artifacts {
    links: vec![
      link("canonical", "https://example.com/a", "https://example.com/a"),
      link("canonical", "https://example.com/a", "https://example.com/a"),
    ],
    url: "https://example.com/a".into(),
    ..Artifacts::default()
  });
  assert!(repeated.passed, "the set is deduplicated before it is counted");
}

#[test]
fn a_canonical_pointing_at_the_domain_root_from_a_subpage_fails() {
  let report = canonical(&Artifacts {
    links: vec![link("canonical", "https://example.com/", "https://example.com/")],
    url: "https://example.com/deep/page".into(),
    ..Artifacts::default()
  });
  assert!(!report.passed);
  assert!(report.explanation.unwrap_or_default().contains("root"));

  // The same canonical is fine when the page IS the root.
  let root = canonical(&Artifacts {
    links: vec![link("canonical", "https://example.com/", "https://example.com/")],
    url: "https://example.com/".into(),
    ..Artifacts::default()
  });
  assert!(root.passed);
}

#[test]
fn a_canonical_pointing_at_another_hreflang_alternate_fails() {
  let mut alternate_self = link("alternate", "https://example.com/en", "https://example.com/en");
  alternate_self.hreflang = "en".into();
  let mut alternate_other = link("alternate", "https://example.com/de", "https://example.com/de");
  alternate_other.hreflang = "de".into();
  let report = canonical(&Artifacts {
    links: vec![
      link("canonical", "https://example.com/de", "https://example.com/de"),
      alternate_self,
      alternate_other,
    ],
    url: "https://example.com/en".into(),
    ..Artifacts::default()
  });
  assert!(!report.passed);
  assert!(report.explanation.unwrap_or_default().contains("hreflang"));
}

#[test]
fn only_narrows_the_run_and_an_empty_list_runs_everything() {
  let all = run(&served(200, Vec::new()), &PageQualityOptions::default());
  assert_eq!(all.audits.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), AUDIT_IDS);

  let one = run(
    &Artifacts::default(),
    &PageQualityOptions {
      only: vec!["doctype".into()],
    },
  );
  assert_eq!(one.audits.len(), 1);
  assert_eq!(one.audits[0].id, "doctype");
}

#[test]
fn a_vector_image_is_skipped_by_both_image_audits() {
  assert_eq!(guess_mime_type("https://example.com/a.svg"), Some("image/svg+xml"));
  assert_eq!(guess_mime_type("data:image/svg+xml;base64,AAA"), Some("image/svg+xml"));
  assert_eq!(guess_mime_type("https://example.com/a.PNG"), Some("image/png"));
  assert_eq!(guess_mime_type("https://example.com/a"), None);
}

// -- http-status-code --------------------------------------------------

#[test]
fn only_the_four_hundreds_and_five_hundreds_fail_the_status_audit() {
  for status in [200, 204, 301, 302, 399, 600] {
    let report = http_status_code(&served(status, Vec::new())).expect("a status is known");
    assert!(report.passed, "{status} should pass");
  }
  for status in [400, 404, 451, 500, 599] {
    let report = http_status_code(&served(status, Vec::new())).expect("a status is known");
    assert!(!report.passed, "{status} should fail");
    assert_eq!(report.explanation.as_deref(), Some(status.to_string().as_str()));
  }
}

#[test]
fn a_document_that_arrived_without_a_response_is_not_scored() {
  // `setContent` and `about:blank` both land here. Reporting a pass
  // would claim a status nothing observed.
  assert!(http_status_code(&Artifacts::default()).is_none());
  assert!(is_crawlable(&Artifacts::default()).is_none());
}

// -- is-crawlable ------------------------------------------------------

fn crawlable(artifacts: &Artifacts) -> AuditResult {
  is_crawlable(artifacts).expect("a main document is known")
}

#[test]
fn a_page_with_nothing_blocking_it_is_crawlable() {
  assert!(crawlable(&served(200, Vec::new())).passed);
}

#[test]
fn a_robots_meta_saying_noindex_blocks_every_bot() {
  let mut artifacts = served(200, Vec::new());
  artifacts.metas = vec![meta("robots", "noindex, nofollow")];
  let report = crawlable(&artifacts);
  assert!(!report.passed);
  assert_eq!(report.items.len(), 1);
  assert_eq!(report.items[0].selector, "meta[name=robots]");
}

#[test]
fn a_bot_specific_meta_lifts_the_generic_one_for_that_bot_alone() {
  // Only one meta is consulted per bot, so `googlebot` naming itself
  // replaces `robots` rather than adding to it -- one bot left
  // crawling is enough to pass.
  let mut artifacts = served(200, Vec::new());
  artifacts.metas = vec![meta("robots", "noindex"), meta("googlebot", "all")];
  assert!(crawlable(&artifacts).passed);
}

#[test]
fn an_unprefixed_robots_header_blocks_and_a_prefixed_one_blocks_its_own_bot() {
  let blocked = crawlable(&served(200, vec![header("X-Robots-Tag", "noindex")]));
  assert!(!blocked.passed);
  assert_eq!(blocked.items.len(), 1);
  assert_eq!(blocked.items[0].snippet, "X-Robots-Tag: noindex");

  // Googlebot is blocked, the other four are not, so the page passes.
  let one_bot = crawlable(&served(200, vec![header("X-Robots-Tag", "Googlebot: noindex")]));
  assert!(one_bot.passed);
  assert_eq!(one_bot.items, [] as [crate::AuditItem; 0]);
}

#[test]
fn every_bot_blocked_by_its_own_header_still_fails() {
  let headers = BOT_USER_AGENTS
    .iter()
    .map(|agent| match agent {
      Some(agent) => header("X-Robots-Tag", &format!("{agent}: noindex")),
      None => header("X-Robots-Tag", "noindex"),
    })
    .collect();
  let report = crawlable(&served(200, headers));
  assert!(!report.passed);
  // The report lists what blocks the GENERIC agent, which is the one
  // unprefixed header and not the four aimed at named bots.
  assert_eq!(report.items.len(), 1);
}

#[test]
fn unavailable_after_blocks_once_the_date_has_gone_by() {
  let past = crawlable(&served(
    200,
    vec![header("X-Robots-Tag", "unavailable_after: 25 Jun 2010 15:00:00 PST")],
  ));
  assert!(!past.passed);

  let future = crawlable(&served(
    200,
    vec![header("X-Robots-Tag", "unavailable_after: 25 Jun 2999 15:00:00 PST")],
  ));
  assert!(future.passed);

  // `unavailable_after:` reads like a user-agent prefix and is not
  // one; mistaking it for one would skip the directive for every bot.
  assert_eq!(header_directive_user_agent("unavailable_after: 25 Jun 2010"), None);
  assert_eq!(
    header_directive_user_agent("Googlebot: noindex"),
    Some("Googlebot".to_string())
  );
  assert_eq!(header_directive_user_agent("noindex"), None);
  assert_eq!(header_directive_user_agent("noindex, nofollow: x"), None);
}

#[test]
fn both_written_date_formats_parse_and_an_unreadable_one_does_not_block() {
  // 2010-06-25T15:00:00-08:00 is 23:00 UTC.
  assert_eq!(parse_directive_date("2010-06-25t15:00:00-08:00"), Some(1_277_506_800));
  assert_eq!(parse_directive_date("25 jun 2010 15:00:00 pst"), Some(1_277_506_800));
  assert_eq!(
    parse_directive_date("fri, 25 jun 2010 23:00:00 gmt"),
    Some(1_277_506_800)
  );
  assert_eq!(parse_directive_date("2010-06-25"), Some(1_277_424_000));
  assert_eq!(parse_directive_date("the day after tomorrow"), None);
  assert!(!is_unavailable("unavailable_after: never"));
}

#[test]
fn a_robots_txt_disallowing_the_page_blocks_every_bot() {
  let mut artifacts = served(200, Vec::new());
  artifacts.robots_txt = Some(RobotsTxt {
    status: Some(200),
    content: Some("User-agent: *\nDisallow: /page".into()),
  });
  let report = crawlable(&artifacts);
  assert!(!report.passed);
  assert_eq!(report.items.len(), 1);
  assert_eq!(report.items[0].snippet, "https://example.com/robots.txt");
  assert_eq!(report.items[0].detail, "line 2");
}

#[test]
fn a_robots_txt_that_leaves_one_bot_alone_does_not_fail_the_page() {
  let mut artifacts = served(200, Vec::new());
  artifacts.robots_txt = Some(RobotsTxt {
    status: Some(200),
    content: Some("User-agent: *\nDisallow: /\n\nUser-agent: bingbot\nDisallow:".into()),
  });
  assert!(crawlable(&artifacts).passed);
}

#[test]
fn a_robots_txt_that_was_not_served_blocks_nothing() {
  let mut artifacts = served(200, Vec::new());
  artifacts.robots_txt = Some(RobotsTxt {
    status: Some(404),
    content: None,
  });
  assert!(crawlable(&artifacts).passed);
}

#[test]
fn robots_txt_sits_at_the_origin_root_or_nowhere() {
  assert_eq!(
    robots_txt_url("https://example.com/a/b?c=d#e").as_deref(),
    Some("https://example.com/robots.txt")
  );
  assert_eq!(robots_txt_url("about:blank"), None);
  assert_eq!(robots_txt_url("data:text/html,<p>x</p>"), None);
  assert_eq!(robots_txt_url("not a url"), None);
}
