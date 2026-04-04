use leptos::prelude::*;

/// A counter component with increment/decrement buttons.
#[component]
pub fn Counter(
  #[prop(default = 0)]
  initial: i32,
) -> impl IntoView {
  let (count, set_count) = signal(initial);

  view! {
    <div class="counter">
      <span id="count">{count}</span>
      <button id="inc" on:click=move |_| set_count.update(|n| *n += 1)>"+"</button>
      <button id="dec" on:click=move |_| set_count.update(|n| *n -= 1)>"-"</button>
    </div>
  }
}
