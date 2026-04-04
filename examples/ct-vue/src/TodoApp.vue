<script setup lang="ts">
import { ref, computed } from 'vue';

interface Todo { id: number; title: string; completed: boolean; }
type Filter = 'all' | 'active' | 'completed';

const todos = ref<Todo[]>([]);
const newTitle = ref('');
const filter = ref<Filter>('all');
const editingId = ref<number | null>(null);
const editText = ref('');
let nextId = 1;

const activeCount = computed(() => todos.value.filter(t => !t.completed).length);
const completedCount = computed(() => todos.value.filter(t => t.completed).length);
const allCompleted = computed(() => todos.value.length > 0 && todos.value.every(t => t.completed));
const filtered = computed(() => todos.value.filter(t =>
  filter.value === 'all' ? true : filter.value === 'active' ? !t.completed : t.completed
));

function addTodo() {
  const title = newTitle.value.trim();
  if (!title) return;
  todos.value.push({ id: nextId++, title, completed: false });
  newTitle.value = '';
}
function toggle(id: number) { const t = todos.value.find(t => t.id === id); if (t) t.completed = !t.completed; }
function remove(id: number) { todos.value = todos.value.filter(t => t.id !== id); }
function startEdit(id: number, title: string) { editingId.value = id; editText.value = title; }
function finishEdit(id: number) {
  const title = editText.value.trim();
  if (!title) { remove(id); } else { const t = todos.value.find(t => t.id === id); if (t) t.title = title; }
  editingId.value = null;
}
function toggleAll() { const v = !allCompleted.value; todos.value.forEach(t => t.completed = v); }
function clearCompleted() { todos.value = todos.value.filter(t => !t.completed); }
</script>

<template>
  <section class="todoapp">
    <header>
      <h1>todos</h1>
      <input id="new-todo" class="new-todo" placeholder="What needs to be done?"
        v-model="newTitle" @keydown.enter="addTodo" />
    </header>
    <template v-if="todos.length > 0">
      <div class="toggle-all-container">
        <input type="checkbox" id="toggle-all" class="toggle-all" :checked="allCompleted" @change="toggleAll" />
        <label for="toggle-all">Mark all as complete</label>
      </div>
      <ul class="todo-list" id="todo-list">
        <li v-for="todo in filtered" :key="todo.id"
          :class="{ completed: todo.completed, editing: editingId === todo.id }">
          <template v-if="editingId === todo.id">
            <input class="edit-input" type="text" v-model="editText"
              @blur="finishEdit(todo.id)" @keydown.enter="finishEdit(todo.id)" autofocus />
          </template>
          <template v-else>
            <input type="checkbox" class="toggle" :checked="todo.completed" @change="toggle(todo.id)" />
            <label @dblclick="startEdit(todo.id, todo.title)">{{ todo.title }}</label>
            <button class="destroy" @click="remove(todo.id)">×</button>
          </template>
        </li>
      </ul>
      <footer class="footer">
        <span id="todo-count">{{ activeCount === 1 ? '1 item left' : `${activeCount} items left` }}</span>
        <ul class="filters">
          <li><a id="filter-all" :class="{ selected: filter === 'all' }" @click="filter = 'all'">All</a></li>
          <li><a id="filter-active" :class="{ selected: filter === 'active' }" @click="filter = 'active'">Active</a></li>
          <li><a id="filter-completed" :class="{ selected: filter === 'completed' }" @click="filter = 'completed'">Completed</a></li>
        </ul>
        <button v-if="completedCount > 0" id="clear-completed" class="clear-completed" @click="clearCompleted">Clear completed</button>
      </footer>
    </template>
  </section>
</template>
