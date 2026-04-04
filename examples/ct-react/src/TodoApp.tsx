import { useState } from "react";

interface Todo {
  id: number;
  title: string;
  completed: boolean;
}

type Filter = "all" | "active" | "completed";

export default function TodoApp() {
  const [todos, setTodos] = useState<Todo[]>([]);
  const [newTitle, setNewTitle] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editText, setEditText] = useState("");
  const [nextId, setNextId] = useState(1);

  const activeCount = todos.filter((t) => !t.completed).length;
  const completedCount = todos.filter((t) => t.completed).length;
  const allCompleted = todos.length > 0 && todos.every((t) => t.completed);

  const filtered = todos.filter((t) =>
    filter === "all" ? true : filter === "active" ? !t.completed : t.completed,
  );

  const addTodo = () => {
    const title = newTitle.trim();
    if (!title) return;
    setTodos([...todos, { id: nextId, title, completed: false }]);
    setNextId(nextId + 1);
    setNewTitle("");
  };

  const toggle = (id: number) =>
    setTodos(todos.map((t) => (t.id === id ? { ...t, completed: !t.completed } : t)));

  const remove = (id: number) => setTodos(todos.filter((t) => t.id !== id));

  const startEdit = (id: number, title: string) => { setEditingId(id); setEditText(title); };

  const finishEdit = (id: number) => {
    const title = editText.trim();
    if (!title) { remove(id); } else {
      setTodos(todos.map((t) => (t.id === id ? { ...t, title } : t)));
    }
    setEditingId(null);
  };

  const toggleAll = () => {
    const newState = !allCompleted;
    setTodos(todos.map((t) => ({ ...t, completed: newState })));
  };

  const clearCompleted = () => setTodos(todos.filter((t) => !t.completed));

  return (
    <section className="todoapp">
      <header>
        <h1>todos</h1>
        <input id="new-todo" className="new-todo" placeholder="What needs to be done?"
          value={newTitle} onChange={(e) => setNewTitle(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && addTodo()} />
      </header>
      {todos.length > 0 && (<>
        <div className="toggle-all-container">
          <input type="checkbox" id="toggle-all" className="toggle-all" checked={allCompleted} onChange={toggleAll} />
          <label htmlFor="toggle-all">Mark all as complete</label>
        </div>
        <ul className="todo-list" id="todo-list">
          {filtered.map((todo) => (
            <li key={todo.id} className={`${todo.completed ? "completed" : ""} ${editingId === todo.id ? "editing" : ""}`}>
              {editingId === todo.id ? (
                <input className="edit-input" type="text" value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onBlur={() => finishEdit(todo.id)}
                  onKeyDown={(e) => e.key === "Enter" && finishEdit(todo.id)}
                  autoFocus />
              ) : (<>
                <input type="checkbox" className="toggle" checked={todo.completed} onChange={() => toggle(todo.id)} />
                <label onDoubleClick={() => startEdit(todo.id, todo.title)}>{todo.title}</label>
                <button className="destroy" onClick={() => remove(todo.id)}>×</button>
              </>)}
            </li>
          ))}
        </ul>
        <footer className="footer">
          <span id="todo-count">{activeCount === 1 ? "1 item left" : `${activeCount} items left`}</span>
          <ul className="filters">
            <li><a id="filter-all" className={filter === "all" ? "selected" : ""} onClick={() => setFilter("all")}>All</a></li>
            <li><a id="filter-active" className={filter === "active" ? "selected" : ""} onClick={() => setFilter("active")}>Active</a></li>
            <li><a id="filter-completed" className={filter === "completed" ? "selected" : ""} onClick={() => setFilter("completed")}>Completed</a></li>
          </ul>
          {completedCount > 0 && <button id="clear-completed" className="clear-completed" onClick={clearCompleted}>Clear completed</button>}
        </footer>
      </>)}
    </section>
  );
}
