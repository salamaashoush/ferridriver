use leptos::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
  pub id: u32,
  pub title: String,
  pub completed: bool,
  pub editing: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Filter {
  All,
  Active,
  Completed,
}

#[component]
pub fn TodoApp() -> impl IntoView {
  let (todos, set_todos) = signal(Vec::<Todo>::new());
  let (filter, set_filter) = signal(Filter::All);
  let (next_id, set_next_id) = signal(1u32);
  let (new_title, set_new_title) = signal(String::new());

  let filtered_todos = move || {
    let todos = todos.get();
    let f = filter.get();
    todos
      .into_iter()
      .filter(|t| match f {
        Filter::All => true,
        Filter::Active => !t.completed,
        Filter::Completed => t.completed,
      })
      .collect::<Vec<_>>()
  };

  let active_count = move || todos.get().iter().filter(|t| !t.completed).count();
  let completed_count = move || todos.get().iter().filter(|t| t.completed).count();
  let all_completed = move || {
    let todos = todos.get();
    !todos.is_empty() && todos.iter().all(|t| t.completed)
  };

  let add_todo = move |_| {
    let title = new_title.get().trim().to_string();
    if title.is_empty() {
      return;
    }
    let id = next_id.get();
    set_next_id.set(id + 1);
    set_todos.update(|todos| {
      todos.push(Todo {
        id,
        title,
        completed: false,
        editing: false,
      });
    });
    set_new_title.set(String::new());
  };

  let toggle_todo = move |id: u32| {
    set_todos.update(|todos| {
      if let Some(todo) = todos.iter_mut().find(|t| t.id == id) {
        todo.completed = !todo.completed;
      }
    });
  };

  let delete_todo = move |id: u32| {
    set_todos.update(|todos| {
      todos.retain(|t| t.id != id);
    });
  };

  let start_editing = move |id: u32| {
    set_todos.update(|todos| {
      for todo in todos.iter_mut() {
        todo.editing = todo.id == id;
      }
    });
  };

  let finish_editing = move |id: u32, new_title: String| {
    set_todos.update(|todos| {
      if let Some(todo) = todos.iter_mut().find(|t| t.id == id) {
        let trimmed = new_title.trim().to_string();
        if trimmed.is_empty() {
          todos.retain(|t| t.id != id);
        } else {
          todo.title = trimmed;
          todo.editing = false;
        }
      }
    });
  };

  let toggle_all = move |_| {
    let all_done = all_completed();
    set_todos.update(|todos| {
      for todo in todos.iter_mut() {
        todo.completed = !all_done;
      }
    });
  };

  let clear_completed = move |_| {
    set_todos.update(|todos| {
      todos.retain(|t| !t.completed);
    });
  };

  view! {
    <section class="todoapp">
      <header>
        <h1>"todos"</h1>
        <input
          class="new-todo"
          id="new-todo"
          placeholder="What needs to be done?"
          prop:value=new_title
          on:input=move |ev| set_new_title.set(event_target_value(&ev))
          on:keydown=move |ev| {
            if ev.key() == "Enter" { add_todo(()); }
          }
        />
      </header>

      <Show when=move || !todos.get().is_empty()>
        <div class="toggle-all-container">
          <input
            type="checkbox"
            class="toggle-all"
            id="toggle-all"
            prop:checked=all_completed
            on:change=toggle_all
          />
          <label for="toggle-all">"Mark all as complete"</label>
        </div>

        <ul class="todo-list" id="todo-list">
          {move || {
            filtered_todos()
              .into_iter()
              .map(|todo| {
                let id = todo.id;
                let completed = todo.completed;
                let editing = todo.editing;
                let title = todo.title.clone();

                if editing {
                  let finish = finish_editing;
                  view! {
                    <li class:completed=completed class:editing=true data-id=id>
                      <input
                        class="edit-input"
                        type="text"
                        value=title
                        on:blur=move |ev| {
                          finish(id, event_target_value(&ev));
                        }
                        on:keydown=move |ev| {
                          if ev.key() == "Enter" {
                            finish(id, event_target_value(&ev));
                          }
                        }
                      />
                    </li>
                  }.into_any()
                } else {
                  view! {
                    <li class:completed=completed data-id=id>
                      <input
                        type="checkbox"
                        class="toggle"
                        prop:checked=completed
                        on:change=move |_| toggle_todo(id)
                      />
                      <label on:dblclick=move |_| start_editing(id)>
                        {title}
                      </label>
                      <button class="destroy" on:click=move |_| delete_todo(id)>
                        "×"
                      </button>
                    </li>
                  }.into_any()
                }
              })
              .collect::<Vec<_>>()
          }}
        </ul>

        <footer class="footer">
          <span id="todo-count">
            {move || {
              let count = active_count();
              if count == 1 { "1 item left".to_string() }
              else { format!("{count} items left") }
            }}
          </span>

          <ul class="filters">
            <li>
              <a
                id="filter-all"
                class:selected=move || filter.get() == Filter::All
                on:click=move |_| set_filter.set(Filter::All)
              >"All"</a>
            </li>
            <li>
              <a
                id="filter-active"
                class:selected=move || filter.get() == Filter::Active
                on:click=move |_| set_filter.set(Filter::Active)
              >"Active"</a>
            </li>
            <li>
              <a
                id="filter-completed"
                class:selected=move || filter.get() == Filter::Completed
                on:click=move |_| set_filter.set(Filter::Completed)
              >"Completed"</a>
            </li>
          </ul>

          <Show when=move || { completed_count() > 0 }>
            <button class="clear-completed" id="clear-completed" on:click=clear_completed>
              "Clear completed"
            </button>
          </Show>
        </footer>
      </Show>
    </section>
  }
}
