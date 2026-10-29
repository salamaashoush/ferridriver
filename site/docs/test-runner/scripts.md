# Plain scripts

A file named by `[test] scriptMatch` runs as a plain script: no import, no
`test()` call, just the steps.

```ts
await page.goto('https://example.com/');
await page.getByRole('link', { name: 'Learn more' }).click();
await expect(page).toHaveURL(/iana/);
```

```toml
[test]
scriptMatch = ["checks/*.ts"]
```

Each script is one test, titled by its file name without the extensions
(`checks/signup.ts` is `signup`). Its body is the file's top level, with
top-level `await`, and these in scope:

- `page`, `context`, `browser` and `request`: the test's own fixtures,
  configured by `use` exactly as a test file's are.
- `expect`, with every matcher.
- `test`, the base test, for `test.step()` and `test.info()`.

Everything a test gets, a script gets: the configured trace, video and
screenshot, retries, `--repeat-each`, every project, every reporter, and
the automatic fixtures and overrides an extension contributes with
`defineFixtures`. Every browser call is a `pw:api` step of the test, and
`test.step()` names a group of them.

A script can import other modules; it is bundled on its own, so an error
or a step names the line of the file it was written in. It is evaluated
afresh for every attempt and repetition, never at collection, so listing
(`ferridriver test --list`) compiles a script and runs none of it.

A script declares no tests: `test('...', ...)` inside one fails the run
and says so. A file both `testMatch` and `scriptMatch` name is a script.
A config that sets only `scriptMatch` discovers no test files, where one
setting neither falls back to `**/*.spec.ts` and `**/*.test.ts`.

## Types

`@ferridriver/test/script` declares the script globals. Reference it from
a script, or name it in a `tsconfig.json`:

```ts
/// <reference types="@ferridriver/test/script" />
```

It is kept apart from `@ferridriver/test` because a test file has none of
these globals; its fixtures arrive as the test function's argument.
