<script lang="ts">
  let todos = $state<{id: number; title: string; completed: boolean}[]>([]);
  let newTitle = $state('');
  let filter = $state<'all'|'active'|'completed'>('all');
  let editingId = $state<number|null>(null);
  let editText = $state('');
  let nextId = 1;

  let activeCount = $derived(todos.filter(t => !t.completed).length);
  let completedCount = $derived(todos.filter(t => t.completed).length);
  let allCompleted = $derived(todos.length > 0 && todos.every(t => t.completed));
  let filtered = $derived(todos.filter(t =>
    filter === 'all' ? true : filter === 'active' ? !t.completed : t.completed
  ));

  function addTodo() {
    const title = newTitle.trim();
    if (!title) return;
    todos = [...todos, { id: nextId++, title, completed: false }];
    newTitle = '';
  }
  function toggle(id: number) { todos = todos.map(t => t.id === id ? {...t, completed: !t.completed} : t); }
  function remove(id: number) { todos = todos.filter(t => t.id !== id); }
  function startEdit(id: number, title: string) { editingId = id; editText = title; }
  function finishEdit(id: number) {
    const title = editText.trim();
    if (!title) remove(id);
    else todos = todos.map(t => t.id === id ? {...t, title} : t);
    editingId = null;
  }
  function toggleAll() { const v = !allCompleted; todos = todos.map(t => ({...t, completed: v})); }
  function clearCompleted() { todos = todos.filter(t => !t.completed); }
</script>

<section class="todoapp">
  <header>
    <h1>todos</h1>
    <input id="new-todo" class="new-todo" placeholder="What needs to be done?"
      bind:value={newTitle} onkeydown={(e) => e.key === 'Enter' && addTodo()} />
  </header>
  {#if todos.length > 0}
    <div class="toggle-all-container">
      <input type="checkbox" id="toggle-all" class="toggle-all" checked={allCompleted} onchange={toggleAll} />
      <label for="toggle-all">Mark all as complete</label>
    </div>
    <ul class="todo-list" id="todo-list">
      {#each filtered as todo (todo.id)}
        <li class:completed={todo.completed} class:editing={editingId === todo.id}>
          {#if editingId === todo.id}
            <input class="edit-input" type="text" bind:value={editText}
              onblur={() => finishEdit(todo.id)} onkeydown={(e) => e.key === 'Enter' && finishEdit(todo.id)} />
          {:else}
            <input type="checkbox" class="toggle" checked={todo.completed} onchange={() => toggle(todo.id)} />
            <label ondblclick={() => startEdit(todo.id, todo.title)}>{todo.title}</label>
            <button class="destroy" onclick={() => remove(todo.id)}>×</button>
          {/if}
        </li>
      {/each}
    </ul>
    <footer class="footer">
      <span id="todo-count">{activeCount === 1 ? '1 item left' : `${activeCount} items left`}</span>
      <ul class="filters">
        <li><a id="filter-all" class:selected={filter === 'all'} onclick={() => filter = 'all'}>All</a></li>
        <li><a id="filter-active" class:selected={filter === 'active'} onclick={() => filter = 'active'}>Active</a></li>
        <li><a id="filter-completed" class:selected={filter === 'completed'} onclick={() => filter = 'completed'}>Completed</a></li>
      </ul>
      {#if completedCount > 0}
        <button id="clear-completed" class="clear-completed" onclick={clearCompleted}>Clear completed</button>
      {/if}
    </footer>
  {/if}
</section>
