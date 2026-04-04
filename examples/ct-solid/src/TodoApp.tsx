import { createSignal, For, Show } from "solid-js";

interface Todo { id: number; title: string; completed: boolean; }
type Filter = "all" | "active" | "completed";

export default function TodoApp() {
  const [todos, setTodos] = createSignal<Todo[]>([]);
  const [newTitle, setNewTitle] = createSignal("");
  const [filter, setFilter] = createSignal<Filter>("all");
  const [editingId, setEditingId] = createSignal<number | null>(null);
  const [editText, setEditText] = createSignal("");
  let nextId = 1;

  const activeCount = () => todos().filter(t => !t.completed).length;
  const completedCount = () => todos().filter(t => t.completed).length;
  const allCompleted = () => todos().length > 0 && todos().every(t => t.completed);
  const filtered = () => todos().filter(t =>
    filter() === "all" ? true : filter() === "active" ? !t.completed : t.completed
  );

  const addTodo = () => {
    const title = newTitle().trim();
    if (!title) return;
    setTodos([...todos(), { id: nextId++, title, completed: false }]);
    setNewTitle("");
  };
  const toggle = (id: number) => setTodos(todos().map(t => t.id === id ? {...t, completed: !t.completed} : t));
  const remove = (id: number) => setTodos(todos().filter(t => t.id !== id));
  const startEdit = (id: number, title: string) => { setEditingId(id); setEditText(title); };
  const finishEdit = (id: number) => {
    const title = editText().trim();
    if (!title) remove(id);
    else setTodos(todos().map(t => t.id === id ? {...t, title} : t));
    setEditingId(null);
  };
  const toggleAll = () => { const v = !allCompleted(); setTodos(todos().map(t => ({...t, completed: v}))); };
  const clearCompleted = () => setTodos(todos().filter(t => !t.completed));

  return (
    <section class="todoapp">
      <header>
        <h1>todos</h1>
        <input id="new-todo" class="new-todo" placeholder="What needs to be done?"
          value={newTitle()} onInput={(e) => setNewTitle(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && addTodo()} />
      </header>
      <Show when={todos().length > 0}>
        <div class="toggle-all-container">
          <input type="checkbox" id="toggle-all" class="toggle-all" checked={allCompleted()} onChange={toggleAll} />
          <label for="toggle-all">Mark all as complete</label>
        </div>
        <ul class="todo-list" id="todo-list">
          <For each={filtered()}>{(todo) =>
            <li classList={{ completed: todo.completed, editing: editingId() === todo.id }}>
              <Show when={editingId() === todo.id} fallback={<>
                <input type="checkbox" class="toggle" checked={todo.completed} onChange={() => toggle(todo.id)} />
                <label onDblClick={() => startEdit(todo.id, todo.title)}>{todo.title}</label>
                <button class="destroy" onClick={() => remove(todo.id)}>×</button>
              </>}>
                <input class="edit-input" type="text" value={editText()}
                  onInput={(e) => setEditText(e.currentTarget.value)}
                  onBlur={() => finishEdit(todo.id)}
                  onKeyDown={(e) => e.key === "Enter" && finishEdit(todo.id)} />
              </Show>
            </li>
          }</For>
        </ul>
        <footer class="footer">
          <span id="todo-count">{activeCount() === 1 ? "1 item left" : `${activeCount()} items left`}</span>
          <ul class="filters">
            <li><a id="filter-all" classList={{ selected: filter() === "all" }} onClick={() => setFilter("all")}>All</a></li>
            <li><a id="filter-active" classList={{ selected: filter() === "active" }} onClick={() => setFilter("active")}>Active</a></li>
            <li><a id="filter-completed" classList={{ selected: filter() === "completed" }} onClick={() => setFilter("completed")}>Completed</a></li>
          </ul>
          <Show when={completedCount() > 0}>
            <button id="clear-completed" class="clear-completed" onClick={clearCompleted}>Clear completed</button>
          </Show>
        </footer>
      </Show>
    </section>
  );
}
