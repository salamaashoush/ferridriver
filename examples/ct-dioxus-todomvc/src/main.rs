use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct Todo {
  id: u32,
  title: String,
  completed: bool,
  editing: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
  All,
  Active,
  Completed,
}

fn main() {
  dioxus::launch(App);
}

#[component]
fn App() -> Element {
  let mut todos = use_signal(Vec::<Todo>::new);
  let mut filter = use_signal(|| Filter::All);
  let mut next_id = use_signal(|| 1u32);
  let mut new_title = use_signal(String::new);

  let filtered: Vec<Todo> = todos.read().iter().filter(|t| match *filter.read() {
    Filter::All => true,
    Filter::Active => !t.completed,
    Filter::Completed => t.completed,
  }).cloned().collect();

  let active_count = todos.read().iter().filter(|t| !t.completed).count();
  let completed_count = todos.read().iter().filter(|t| t.completed).count();
  let all_completed = !todos.read().is_empty() && todos.read().iter().all(|t| t.completed);

  rsx! {
    document::Link { rel: "stylesheet", href: asset!("/assets/style.css") }
    section { class: "todoapp",
      header {
        h1 { "todos" }
        input {
          class: "new-todo",
          id: "new-todo",
          placeholder: "What needs to be done?",
          value: "{new_title}",
          oninput: move |ev| new_title.set(ev.value()),
          onkeydown: move |ev| {
            if ev.key() == Key::Enter {
              let title = new_title.read().trim().to_string();
              if !title.is_empty() {
                let id = *next_id.read();
                next_id.set(id + 1);
                todos.write().push(Todo { id, title, completed: false, editing: false });
                new_title.set(String::new());
              }
            }
          }
        }
      }

      if !todos.read().is_empty() {
        div { class: "toggle-all-container",
          input {
            r#type: "checkbox",
            class: "toggle-all",
            id: "toggle-all",
            checked: all_completed,
            onchange: move |_| {
              let new_state = !all_completed;
              for todo in todos.write().iter_mut() {
                todo.completed = new_state;
              }
            }
          }
          label { r#for: "toggle-all", "Mark all as complete" }
        }

        ul { class: "todo-list", id: "todo-list",
          for todo in filtered.iter() {
            {
              let id = todo.id;
              let completed = todo.completed;
              let editing = todo.editing;
              let title = todo.title.clone();

              if editing {
                rsx! {
                  li { class: if completed { "completed editing" } else { "editing" },
                    "data-id": "{id}",
                    input {
                      class: "edit-input",
                      r#type: "text",
                      value: "{title}",
                      oninput: move |ev| {
                        // Store the current input value so blur/enter can read it.
                        if let Some(t) = todos.write().iter_mut().find(|t| t.id == id) {
                          t.title = ev.value();
                        }
                      },
                      onblur: move |_| {
                        let mut list = todos.write();
                        let title = list.iter().find(|t| t.id == id).map(|t| t.title.trim().to_string()).unwrap_or_default();
                        if title.is_empty() {
                          list.retain(|t| t.id != id);
                        } else if let Some(t) = list.iter_mut().find(|t| t.id == id) {
                          t.editing = false;
                        }
                      },
                      onkeydown: move |ev| {
                        if ev.key() == Key::Enter {
                          let mut list = todos.write();
                          let title = list.iter().find(|t| t.id == id).map(|t| t.title.trim().to_string()).unwrap_or_default();
                          if title.is_empty() {
                            list.retain(|t| t.id != id);
                          } else if let Some(t) = list.iter_mut().find(|t| t.id == id) {
                            t.editing = false;
                          }
                        }
                      }
                    }
                  }
                }
              } else {
                rsx! {
                  li { class: if completed { "completed" } else { "" },
                    "data-id": "{id}",
                    input {
                      r#type: "checkbox",
                      class: "toggle",
                      checked: completed,
                      onchange: move |_| {
                        if let Some(t) = todos.write().iter_mut().find(|t| t.id == id) {
                          t.completed = !t.completed;
                        }
                      }
                    }
                    label {
                      ondoubleclick: move |_| {
                        for t in todos.write().iter_mut() {
                          t.editing = t.id == id;
                        }
                      },
                      "{title}"
                    }
                    button { class: "destroy",
                      onclick: move |_| { todos.write().retain(|t| t.id != id); },
                      "×"
                    }
                  }
                }
              }
            }
          }
        }

        footer { class: "footer",
          span { id: "todo-count",
            if active_count == 1 { "1 item left" } else { "{active_count} items left" }
          }
          ul { class: "filters",
            li {
              a { id: "filter-all",
                class: if *filter.read() == Filter::All { "selected" } else { "" },
                onclick: move |_| filter.set(Filter::All),
                "All"
              }
            }
            li {
              a { id: "filter-active",
                class: if *filter.read() == Filter::Active { "selected" } else { "" },
                onclick: move |_| filter.set(Filter::Active),
                "Active"
              }
            }
            li {
              a { id: "filter-completed",
                class: if *filter.read() == Filter::Completed { "selected" } else { "" },
                onclick: move |_| filter.set(Filter::Completed),
                "Completed"
              }
            }
          }
          if completed_count > 0 {
            button { class: "clear-completed", id: "clear-completed",
              onclick: move |_| { todos.write().retain(|t| !t.completed); },
              "Clear completed"
            }
          }
        }
      }
    }
  }
}
