# Component testing

Mount React, Vue, Svelte, or Solid components in a real browser and interact with them using the same Page / Locator API as your E2E tests.

## How it works

1. The `ct` CLI starts a Vite dev server at `http://localhost:3100`.
2. `@ferridriver/ct-core` scans your test files and rewrites component imports to lazy `import()` references.
3. For each test, a fresh page navigates to the CT preview server, and `mount(Component, { props })` serializes the component tree into the browser, where the framework-specific `__ferriMount` handler renders it.
4. `expect` / `Locator` drive the mounted component just like any other page.

## Install

Pick your framework:

```bash
npm install -D @ferridriver/test @ferridriver/ct-react react react-dom
npm install -D @ferridriver/test @ferridriver/ct-vue vue
npm install -D @ferridriver/test @ferridriver/ct-svelte svelte
npm install -D @ferridriver/test @ferridriver/ct-solid solid-js
```

## Run

```bash
npx @ferridriver/test ct --framework react   src/**/*.ct.tsx
npx @ferridriver/test ct --framework vue     src/**/*.ct.ts
npx @ferridriver/test ct --framework svelte  src/**/*.ct.ts
npx @ferridriver/test ct --framework solid   src/**/*.ct.tsx
```

## Framework guides

- [React](/component-testing/react)
- [Vue](/component-testing/vue)
- [Svelte](/component-testing/svelte) (4 and 5)
- [Solid](/component-testing/solid)
