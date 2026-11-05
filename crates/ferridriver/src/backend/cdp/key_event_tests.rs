use ferridriver_types::keyboard::Keyboard;
use serde_json::json;

use super::{key_down_params, key_up_params};

#[test]
fn a_held_modifier_rides_on_every_later_key_event() {
  let mut keyboard = Keyboard::new();
  let shift = keyboard.down("Shift", false).unwrap();
  assert_eq!(
    key_down_params(&shift, false),
    json!({
      "type": "rawKeyDown", "modifiers": 8, "windowsVirtualKeyCode": 16, "code": "ShiftLeft",
      "commands": [], "key": "Shift", "text": "", "unmodifiedText": "", "autoRepeat": false,
      "location": 1, "isKeypad": false,
    })
  );
  let a = keyboard.down("KeyA", false).unwrap();
  assert_eq!(
    key_down_params(&a, false),
    json!({
      "type": "keyDown", "modifiers": 8, "windowsVirtualKeyCode": 65, "code": "KeyA",
      "commands": [], "key": "A", "text": "A", "unmodifiedText": "A", "autoRepeat": false,
      "location": 0, "isKeypad": false,
    })
  );
  let a = keyboard.up("KeyA", false).unwrap();
  assert_eq!(
    key_up_params(&a),
    json!({
      "type": "keyUp", "modifiers": 8, "key": "A", "windowsVirtualKeyCode": 65, "code": "KeyA",
      "location": 0, "isKeypad": false,
    })
  );
  let shift = keyboard.up("Shift", false).unwrap();
  assert_eq!(key_up_params(&shift)["modifiers"], 0);
}

#[test]
fn a_shortcut_types_nothing_and_names_its_mac_command() {
  let mut keyboard = Keyboard::new();
  keyboard.down("Meta", true).unwrap();
  let a = keyboard.down("a", true).unwrap();
  let mac = key_down_params(&a, true);
  assert_eq!(mac["type"], "rawKeyDown");
  assert_eq!(mac["text"], "");
  assert_eq!(mac["modifiers"], 4);
  assert_eq!(mac["commands"], json!(["selectAll"]));
  assert_eq!(key_down_params(&a, false)["commands"], json!([]));
}

#[test]
fn keypad_keys_and_repeats_are_marked() {
  let mut keyboard = Keyboard::new();
  let end = keyboard.down("Numpad1", false).unwrap();
  let params = key_down_params(&end, false);
  assert_eq!(
    (params["key"].clone(), params["location"].clone()),
    (json!("End"), json!(3))
  );
  assert_eq!(params["isKeypad"], true);
  assert_eq!(params["windowsVirtualKeyCode"], 35);
  let again = keyboard.down("Numpad1", false).unwrap();
  assert_eq!(key_down_params(&again, false)["autoRepeat"], true);
}
