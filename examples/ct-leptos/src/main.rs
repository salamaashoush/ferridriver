use ct_leptos_example::Counter;
use leptos::prelude::*;

fn main() {
  console_error_panic_hook::set_once();
  mount_to_body(|| view! { <Counter initial=0 /> });
}
