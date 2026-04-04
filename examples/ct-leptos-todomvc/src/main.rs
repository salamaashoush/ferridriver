use ct_leptos_todomvc::TodoApp;
use leptos::prelude::*;

fn main() {
  console_error_panic_hook::set_once();
  mount_to_body(TodoApp);
}
