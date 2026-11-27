//! Unit tests for the codegen recorder.

use super::*;

fn shared() -> (Arc<Shared>, Arc<Mutex<Vec<RecorderEvent>>>) {
  let seen = Arc::new(Mutex::new(Vec::new()));
  let sink_seen = Arc::clone(&seen);
  let shared = Arc::new(Shared {
    sink: Box::new(move |event| sink_seen.lock().unwrap().push(event)),
    state: Mutex::new(State::default()),
  });
  (shared, seen)
}

fn message(action: &serde_json::Value) -> serde_json::Value {
  serde_json::json!({ "kind": "action", "action": action })
}

#[test]
fn modes_round_trip_through_their_wire_names() {
  for name in [
    "recording",
    "none",
    "assertingVisibility",
    "assertingText",
    "assertingValue",
  ] {
    let mode = RecorderMode::parse(name).unwrap();
    assert_eq!(mode.as_str(), name);
    assert_eq!(serde_json::to_value(mode).unwrap(), name);
  }
  assert!(RecorderMode::parse("inspecting").unwrap_err().contains("assertingText"));
}

#[test]
fn a_double_click_replaces_the_click_it_began_as() {
  let (shared, seen) = shared();
  let button = serde_json::json!({ "selector": "#b", "locator": "locator('#b')" });
  let mut click = button.clone();
  click["type"] = "click".into();
  let mut dblclick = button;
  dblclick["type"] = "dblclick".into();
  shared.on_message(FIRST_PAGE, &message(&click));
  shared.on_message(FIRST_PAGE, &message(&dblclick));
  assert_eq!(
    shared.state().actions,
    [RecordedAction::on_first_page(Action::Dblclick {
      selector: "#b".into(),
      locator: "locator('#b')".into(),
      options: super::super::ClickOptions::default(),
    })]
  );
  let flags: Vec<(bool, Option<usize>)> = seen
    .lock()
    .unwrap()
    .iter()
    .filter_map(|event| match event {
      RecorderEvent::Action {
        replaces_previous,
        replaces_index,
        ..
      } => Some((*replaces_previous, *replaces_index)),
      _ => None,
    })
    .collect();
  assert_eq!(flags, [(false, None), (true, Some(0))]);
}

#[test]
fn a_popup_announced_late_replaces_the_entry_that_opened_it() {
  let click = |page: &str, selector: &str| RecordedAction {
    page: page.into(),
    action: Action::Click {
      selector: selector.into(),
      locator: format!("locator('{selector}')"),
      options: super::super::ClickOptions::default(),
    },
    popup: None,
  };
  let mut state = State {
    actions: vec![click(FIRST_PAGE, "#later"), click("page1", "#other")],
    ..State::default()
  };
  let Some(RecorderEvent::Action {
    action,
    replaces_previous,
    replaces_index,
  }) = state.open_popup(FIRST_PAGE, "page2")
  else {
    panic!("the opener's action was not replaced");
  };
  assert_eq!((replaces_previous, replaces_index), (false, Some(0)));
  assert_eq!(action.popup.as_deref(), Some("page2"));
  assert_eq!(state.actions[0].popup.as_deref(), Some("page2"));
  assert!(matches!(
    state.open_popup("page1", "page3"),
    Some(RecorderEvent::Action {
      replaces_previous: true,
      replaces_index: Some(1),
      ..
    })
  ));
  assert!(state.open_popup("page4", "page5").is_none());
}

#[test]
fn a_triple_click_folds_into_one_click_and_two_clicks_stay_two() {
  let (shared, _) = shared();
  let on = |kind: &str, options: serde_json::Value| serde_json::json!({ "type": kind, "selector": "#a", "locator": "locator('#a')", "options": options });
  shared.on_message(FIRST_PAGE, &message(&on("click", serde_json::json!({}))));
  shared.on_message(FIRST_PAGE, &message(&on("dblclick", serde_json::json!({}))));
  shared.on_message(
    FIRST_PAGE,
    &message(&on("click", serde_json::json!({ "clickCount": 3 }))),
  );
  shared.on_message(
    FIRST_PAGE,
    &message(&on("click", serde_json::json!({ "modifiers": ["Shift"] }))),
  );
  shared.on_message(
    FIRST_PAGE,
    &message(&on("click", serde_json::json!({ "button": "right" }))),
  );
  let options: Vec<serde_json::Value> = shared
    .state()
    .actions
    .iter()
    .map(|action| serde_json::to_value(action).unwrap())
    .map(|value| value.get("options").cloned().unwrap_or_default())
    .collect();
  assert_eq!(
    options,
    [
      serde_json::json!({ "clickCount": 3 }),
      serde_json::json!({ "modifiers": ["Shift"] }),
      serde_json::json!({ "button": "right" }),
    ]
  );
}

#[test]
fn an_assertion_returns_the_recorder_to_recording() {
  let (shared, seen) = shared();
  shared.set_mode(RecorderMode::AssertingVisibility);
  let mode = shared.on_message(
    FIRST_PAGE,
    &message(&serde_json::json!({
      "type": "assertVisible", "selector": "#ok", "locator": "locator('#ok')",
    })),
  );
  assert_eq!(mode, RecorderMode::Recording);
  let seen = seen.lock().unwrap();
  assert_eq!(
    seen.first(),
    Some(&RecorderEvent::Mode(RecorderMode::AssertingVisibility))
  );
  assert_eq!(seen.last(), Some(&RecorderEvent::Mode(RecorderMode::Recording)));
}

#[test]
fn an_action_in_a_frame_reaches_in_through_the_frame_elements() {
  let path = ["iframe[title=\"Coupon\"]".to_string(), "#inner".to_string()];
  let selector = "internal:role=textbox[name=\"Code\"i]";
  let full = through_frames(&path, selector);
  assert_eq!(
    full,
    "iframe[title=\"Coupon\"] >> internal:control=enter-frame >> #inner >> internal:control=enter-frame >> \
     internal:role=textbox[name=\"Code\"i]"
  );
  assert_eq!(
    ferridriver::locator_generator::as_locator(&full),
    "locator('iframe[title=\"Coupon\"]').contentFrame().locator('#inner').contentFrame().getByRole('textbox', { name: 'Code' })"
  );
  assert_eq!(through_frames(&[], selector), selector);
  assert_eq!(quote_css_attribute_value("a\"b\\c"), "\"a\\\"b\\\\c\"");
}

#[test]
fn a_deep_path_is_tried_from_any_frame_shortest_first() {
  let selector = "internal:role=button[name=\"Deep\"i]";
  let one = ["#a".to_string()];
  assert_eq!(any_frame_candidates(&one, selector), Vec::<String>::new());
  let three = ["#a".to_string(), "#b".to_string(), "#c".to_string()];
  let candidates = any_frame_candidates(&three, selector);
  assert_eq!(
    candidates,
    [
      "internal:control=any-frame >> internal:role=button[name=\"Deep\"i]",
      "internal:control=any-frame >> #c >> internal:control=enter-frame >> internal:role=button[name=\"Deep\"i]",
    ]
  );
  assert_eq!(
    ferridriver::locator_generator::as_locator(&candidates[0]),
    "frameLocator().getByRole('button', { name: 'Deep' })"
  );
  assert_eq!(
    ferridriver::locator_generator::as_locator(&candidates[1]),
    "frameLocator().locator('#c').contentFrame().getByRole('button', { name: 'Deep' })"
  );
}

fn fill(selector: &str, value: &str, field: Option<serde_json::Value>) -> serde_json::Value {
  let mut message = message(&serde_json::json!({
    "type": "fill", "selector": selector, "locator": format!("locator('{selector}')"), "value": value,
  }));
  if let Some(field) = field {
    message["secretField"] = field;
  }
  message
}

fn secret_events(seen: &[RecorderEvent]) -> Vec<(String, String)> {
  seen
    .iter()
    .filter_map(|event| match event {
      RecorderEvent::Secret { name, value } => Some((name.clone(), value.clone())),
      _ => None,
    })
    .collect()
}

#[test]
fn a_secret_field_is_named_after_its_label_name_id_or_kind() {
  assert_eq!(env_name("Password").as_deref(), Some("PASSWORD"));
  assert_eq!(env_name("  Card number: ").as_deref(), Some("CARD_NUMBER"));
  assert_eq!(env_name("2fa-code").as_deref(), Some("_2FA_CODE"));
  assert_eq!(env_name("Mot de passe é").as_deref(), Some("MOT_DE_PASSE"));
  assert_eq!(env_name("***"), None);
  let field = |label: &str, name: &str, id: &str, password: bool| SecretField {
    password,
    label: label.into(),
    name: name.into(),
    id: id.into(),
  };
  assert_eq!(field("Your password", "pw", "p", true).base_name(), "YOUR_PASSWORD");
  assert_eq!(field("", "user-pw", "p", true).base_name(), "USER_PW");
  assert_eq!(field("", "", "otp", false).base_name(), "OTP");
  assert_eq!(field("", "", "", true).base_name(), "PASSWORD");
  assert_eq!(field("", "", "", false).base_name(), "SECRET");
  assert!(is_env_name("PIN_2") && is_env_name("_x") && !is_env_name("2X") && !is_env_name("A-B"));
}

#[test]
fn a_typed_secret_is_named_and_sent_once_it_is_settled() {
  let (shared, seen) = shared();
  shared.state().secrets = Secrets::Env;
  let field = serde_json::json!({ "password": true, "label": "Password", "name": "", "id": "pw" });
  for typed in ["h", "hu", "hunter2"] {
    shared.on_message(FIRST_PAGE, &fill("#pw", typed, Some(field.clone())));
  }
  assert!(
    secret_events(&seen.lock().unwrap()).is_empty(),
    "nothing is sent while typing"
  );
  // The same value again reuses the name; another value takes the next.
  shared.on_message(FIRST_PAGE, &fill("#again", "hunter2", Some(field.clone())));
  shared.on_message(FIRST_PAGE, &fill("#new", "s3cret", Some(field)));
  // A value typed into a plain field stays as typed.
  shared.on_message(FIRST_PAGE, &fill("#user", "sashoush", None));
  let actions: Vec<serde_json::Value> = shared
    .state()
    .actions
    .iter()
    .map(|action| serde_json::to_value(action).unwrap())
    .collect();
  assert_eq!(
    actions[0],
    serde_json::json!({"type": "fill", "selector": "#pw", "locator": "locator('#pw')", "value": null, "secret": "PASSWORD", "page": "page"})
  );
  assert_eq!(actions[1]["secret"], "PASSWORD");
  assert_eq!(actions[2]["secret"], "PASSWORD_2");
  assert_eq!(actions[3]["value"], "sashoush");
  assert!(actions[3].get("secret").is_none());
  let seen = seen.lock().unwrap();
  assert_eq!(
    secret_events(&seen),
    [
      ("PASSWORD".to_string(), "hunter2".to_string()),
      ("PASSWORD_2".to_string(), "s3cret".to_string()),
    ]
  );
  // Each value is sent before the step that settled it, and no other
  // event carries one.
  let secret_at = seen
    .iter()
    .position(|e| matches!(e, RecorderEvent::Secret { .. }))
    .unwrap();
  assert!(
    matches!(&seen[secret_at + 1], RecorderEvent::Action { action, .. } if action.action.selector() == Some("#again"))
  );
  for event in seen.iter().filter(|e| !matches!(e, RecorderEvent::Secret { .. })) {
    let shown = format!("{event:?}");
    assert!(!shown.contains("hunter") && !shown.contains("s3cret"), "{shown}");
  }
  drop(seen);
  assert_eq!(
    shared.state().secrets_in_use(),
    [
      RecordedSecret {
        name: "PASSWORD".into(),
        value: "hunter2".into()
      },
      RecordedSecret {
        name: "PASSWORD_2".into(),
        value: "s3cret".into()
      },
    ]
  );
}

#[test]
fn without_secrets_a_secret_field_records_its_value() {
  let (shared, seen) = shared();
  let field = serde_json::json!({ "password": true, "label": "Password" });
  shared.on_message(FIRST_PAGE, &fill("#pw", "hunter2", Some(field)));
  assert_eq!(shared.state().actions[0].action.typed_value(), Some("hunter2"));
  assert_eq!(secret_events(&seen.lock().unwrap()), []);
  assert!(shared.state().make_secret(0, "PASSWORD").is_err());
}

#[test]
fn a_step_the_host_names_is_rewritten_with_every_later_one_typing_its_value() {
  let (shared, seen) = shared();
  shared.state().secrets = Secrets::Env;
  shared.on_message(FIRST_PAGE, &fill("#before", "4321", None));
  shared.on_message(FIRST_PAGE, &fill("#pin", "4321", None));
  shared.on_message(FIRST_PAGE, &fill("#user", "sashoush", None));
  shared.on_message(FIRST_PAGE, &fill("#again", "4321", None));
  let mut state = shared.state();
  assert!(state.make_secret(1, "2PIN").is_err(), "not a variable name");
  assert!(state.make_secret(2, "USER").is_ok());
  assert!(state.make_secret(3, "USER").is_err(), "USER stands for another value");
  let (rewritten, events) = state.make_secret(1, "PIN").unwrap();
  assert_eq!(rewritten, [1, 3]);
  assert!(matches!(&events[0], RecorderEvent::Secret { name, value } if name == "PIN" && value == "4321"));
  let replaced: Vec<(bool, Option<usize>)> = events[1..]
    .iter()
    .map(|event| match event {
      RecorderEvent::Action {
        replaces_previous,
        replaces_index,
        ..
      } => (*replaces_previous, *replaces_index),
      other => panic!("{other:?}"),
    })
    .collect();
  assert_eq!(replaced, [(false, Some(1)), (true, Some(3))]);
  assert_eq!(
    state.actions[0].action.typed_value(),
    Some("4321"),
    "an earlier step stays"
  );
  assert_eq!(state.actions[3].action.secret(), Some("PIN"));
  assert_eq!(state.make_secret(1, "PIN").unwrap().0, Vec::<usize>::new());
  drop(state);
  // A later value typed into the named field is a secret of that name.
  shared.on_message(FIRST_PAGE, &fill("#pin", "9999", None));
  assert_eq!(shared.state().actions[4].action.secret(), Some("PIN_2"));
  drop(seen);
}

#[test]
fn a_detached_recorder_records_nothing() {
  let (shared, seen) = shared();
  shared.state().detached = true;
  let mode = shared.on_message(
    FIRST_PAGE,
    &message(&serde_json::json!({ "type": "navigate", "url": "https://example.com/" })),
  );
  assert_eq!(mode, RecorderMode::None);
  assert!(seen.lock().unwrap().is_empty());
}
