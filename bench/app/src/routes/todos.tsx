import { useEffect, useMemo, useState } from 'react';

interface Todo {
  id: string;
  text: string;
  done: boolean;
}

type Filter = 'all' | 'active' | 'completed';

function loadInit(): Todo[] {
  // Read seed query string `?seed=N` so each test can drive a deterministic
  // starting state without leaking between tests.
  const params = new URLSearchParams(window.location.search);
  const n = Number(params.get('seed') ?? '0');
  return Array.from({ length: n }, (_, i) => ({
    id: `seed-${i}`,
    text: `Seed item ${i}`,
    done: i % 3 === 0,
  }));
}

let nextId = 0;

export function TodosPage() {
  const [items, setItems] = useState<Todo[]>(() => loadInit());
  const [draft, setDraft] = useState('');
  const [filter, setFilter] = useState<Filter>('all');

  // Reset between routes — bench tests rely on starting fresh.
  useEffect(() => {
    nextId = items.length;
  }, []);

  const filtered = useMemo(() => {
    if (filter === 'active') return items.filter((t) => !t.done);
    if (filter === 'completed') return items.filter((t) => t.done);
    return items;
  }, [items, filter]);

  const remaining = items.filter((t) => !t.done).length;

  function add() {
    const text = draft.trim();
    if (!text) return;
    nextId++;
    setItems((cur) => [...cur, { id: `t-${nextId}`, text, done: false }]);
    setDraft('');
  }

  return (
    <div className="space-y-4">
      <div className="card">
        <h1 className="text-2xl font-semibold mb-3" data-testid="todos-title">
          Todos
        </h1>
        <div className="flex gap-2">
          <input
            className="input"
            value={draft}
            data-testid="todo-input"
            placeholder="What needs doing?"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') add();
            }}
          />
          <button className="btn" data-testid="todo-add" onClick={add}>
            Add
          </button>
        </div>
        <div className="flex gap-2 mt-3 text-sm">
          {(['all', 'active', 'completed'] as Filter[]).map((f) => (
            <button
              key={f}
              className={`btn-ghost btn ${filter === f ? '!bg-[var(--accent)] !text-white' : ''}`}
              data-testid={`filter-${f}`}
              onClick={() => setFilter(f)}
            >
              {f}
            </button>
          ))}
          <span
            className="ml-auto self-center text-[var(--fg)]/70"
            data-testid="remaining-count"
          >
            {remaining} left
          </span>
        </div>
      </div>

      <ul className="space-y-2" data-testid="todo-list">
        {filtered.map((t) => (
          <li
            key={t.id}
            data-testid={`todo-${t.id}`}
            className="card flex items-center gap-3"
          >
            <input
              type="checkbox"
              checked={t.done}
              data-testid={`toggle-${t.id}`}
              onChange={(e) =>
                setItems((cur) =>
                  cur.map((x) => (x.id === t.id ? { ...x, done: e.target.checked } : x)),
                )
              }
            />
            <span
              className={t.done ? 'line-through text-[var(--fg)]/60' : ''}
              data-testid={`text-${t.id}`}
            >
              {t.text}
            </span>
            <button
              className="btn btn-danger ml-auto"
              data-testid={`delete-${t.id}`}
              onClick={() => setItems((cur) => cur.filter((x) => x.id !== t.id))}
            >
              delete
            </button>
          </li>
        ))}
        {filtered.length === 0 && (
          <li className="text-[var(--fg)]/60 text-center py-6" data-testid="empty">
            No items.
          </li>
        )}
      </ul>
    </div>
  );
}
