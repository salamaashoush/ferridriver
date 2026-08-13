# Named sessions (`run --session`)

A **named session** is a browser that outlives the command that opened
it. You open it once, then run script after script against it from your
terminal — each run sees the same pages, cookies, storage and `globalThis`
the previous one left behind.

This is the CLI counterpart to the MCP server: same engine, same globals,
same sandbox, no MCP client required.

```bash
# open a browser and publish it under an id
ferridriver session open work https://example.com

# run scripts against it — the page is already there
ferridriver run --session work --eval "return page.url()"
ferridriver run --session work --eval "await page.click('text=Sign in'); return page.url()"
ferridriver run --session work login.ts -- alice@example.com

# see what is live, then shut it down
ferridriver session list
ferridriver session close work
```

## Why it is a script, not a verb

The session protocol carries **one** command: run this script. There is no
`click` verb, no `fill` verb, no `snapshot` verb — because every one of
them would be a worse version of the scripting API you already have, and
would drift behind it. `session attach` is itself just a script that
returns `page.snapshotForAI()`.

That means everything a local `ferridriver run` can do, a session run can
do: loops, conditionals, `try`/`catch`, computed values, `expect`
assertions, `request` HTTP calls, extension `tools.*`.

## What runs where

| | Client (`ferridriver run`) | Host (`ferridriver session open`) |
|---|---|---|
| Bundling (rolldown, TypeScript) | yes | — |
| Compile to bytecode | — | yes |
| Browser, pages, cookies | — | yes |
| `fs` / `artifacts` sandbox roots | — | yes |
| Extensions (`tools.*`) | — | yes |
| Console rendering | yes | — |

The split is deliberate. **Bundling is the client's job** because relative
imports and `node_modules` resolve against the directory you typed the
command in — the host cannot know it. **Compiling is the host's job**
because the bytecode must be loaded by the host's own QuickJS build; only
bundled source crosses the wire, never bytecode.

So a TypeScript file with imports works exactly as it does locally:

```ts
// login.ts
import { credentials } from "./fixtures/users";

await page.fill("#email", credentials.email);
await page.fill("#password", credentials.password);
await page.click("button[type=submit]");
export default await page.title();
```

```bash
ferridriver run --session work login.ts
```

## Console streams

`console.log` inside a session run reaches your terminal *while the script
is still running*, on the stream Node would use (`log`/`info`/`debug` to
stdout, `warn`/`error`/`trace` to stderr) — the host emits an event per
line rather than handing back a block at the end.

`--json` still emits exactly one document: the client folds the lines it
streamed into the result's `console` array.

```bash
ferridriver run --session work --json --eval "console.log('hi'); return 1"
```

```jsonc
{ "status": "ok", "value": 1, "duration_ms": 4,
  "console": [{ "level": "log", "message": "hi", "ts_ms": 1 }] }
```

## Watching what the script does

`--trace` streams every browser action back as it happens — start, call log,
duration — the same lines a local run prints:

```bash
ferridriver run -s work --trace --eval "await page.locator('button').click()"
```

```
› locator.click button
  · waiting for locator('button')
✓ locator.click 39ms
```

The host scopes the observer to your session key, so two clients tracing two
contexts of one session never see each other's actions. A run that does not
ask for it installs nothing.

## Turning a session into a test

`--code` renders each action the script performed as source. That is the whole
of "codegen by doing": drive the app, keep what it wrote.

```bash
ferridriver run -s work --code --eval "await page.goto('/login'); await page.locator('#email').fill('a@b.c')"
```

```js
await page.goto('/login');
await page.locator('#email').fill('a@b.c');
```

`--code rust` and `--code gherkin` render the same actions in the other two
surfaces — a `#[ferritest]` body or `.feature` steps:

```
page.locator("#email").fill("a@b.c").await?;
When I fill "#email" with "a@b.c"
```

`--code-out <file>` writes the lines wrapped in that language's scaffolding,
producing a file that runs unchanged both standalone and against a session:

```bash
ferridriver run -s work --code-out login.ts --eval "…"
ferridriver run login.ts             # launches its own browser
ferridriver run -s work login.ts     # reuses the session's page
```

Both work because the scaffolding defines `page` only when there isn't one,
and every emitted line drives plain `page`.

## The response contract (`--report`)

A run tells you what it returned. `--report` also tells you what state you are
now in, as titled sections an agent can skim without parsing anything:

```bash
ferridriver run -s work --report --code --eval "await page.goto('https://example.com/login')"
```

````
### Result
done

### Ran ferridriver code
```ts
await page.goto('https://example.com/login');
```

### Page
- Page URL: https://example.com/login
- Page Title: Sign in
- Console: 1 errors, 0 warnings
````

Sections appear only when they have something to say: no `### Error` on
success, no `### Ran ferridriver code` without `--code`, no console line when
the page logged nothing.

The `### Page` section is read live in the host once the script finishes, so
it describes where the run *left* the session — the page your next command
will act on. Reading it costs one round-trip, which is why a run that does not
pass `--report` does not pay for it.

With `--json` the same parts are folded into the result document under
`report` instead of printed, so a machine consumer still reads exactly one
object.

A local `ferridriver run` (no `--session`) reports everything except the page:
the script owns the browser it launched, and the CLI holds no handle to it.

## Secrets

Declared secrets never reach a caller verbatim. Name them in config — values
live in a dotenv file or the environment, not in the document:

```toml
[secrets]
file = ".env.secrets"          # NAME=value per line, relative to this config
env = ["APP_PASSWORD"]         # or read from the environment by name
```

Every route out is covered: a returned value, a console line the script wrote,
a page URL, an error message, and the code frame around a failure all come
back with the value replaced by `<secret>NAME</secret>`.

Echoed code goes one better and reads the value from the environment, so the
generated file is committable:

```bash
ferridriver run -s work --code --eval "await page.locator('#pw').fill(args[0])" -- "$APP_PASSWORD"
```

```js
await page.locator('#pw').fill(process.env['APP_PASSWORD']);
```

`--code rust` renders `std::env::var("APP_PASSWORD")`; Gherkin renders the
placeholder `<APP_PASSWORD>`.

This is a convenience, not a security boundary: only the values you declare
are matched, and a value the page reshapes (re-encoded, split, embedded in a
token) passes through. Redaction happens in the host, before the wire, so a
client never receives the secret in the first place.

Note that `ferridriver session open` forwards the `--config` and
`--no-inherit` you passed it to the host it spawns — otherwise the session
would run under a different configuration than the command that opened it.

## Keeping the artifacts directory bounded

A session that stays open for days accumulates screenshots, PDFs and traces
from runs whose results were read and forgotten. Give the directory a ceiling:

```toml
artifactsRoot = ".ferridriver/artifacts"
artifactsMaxBytes = 536870912          # 512 MiB
```

After each run that writes an artifact, least-recently-modified files are
deleted until the directory fits again — never the ones that run just wrote,
so a path you were handed still resolves. Unset means no ceiling.

## State between runs

Everything in [State and sessions](/scripting/state-and-sessions) applies:
`globalThis` is shared working space for the session, `vars` is the durable
string store that survives a VM reset, and browser handles are re-resolved
per run so they are never stale.

```bash
ferridriver run -s work --eval "globalThis.count = 1; return null"
ferridriver run -s work --eval "return ++globalThis.count"   # 2
```

## Contexts

`--context` selects a browser context inside the session — its own cookies
and storage, its own VM, its own `vars`.

```bash
ferridriver run -s work --context admin --eval "return context.cookies()"
ferridriver run -s work --context guest --eval "return context.cookies()"
```

## Extensions belong to the host

Extensions are loaded once, by the session host, so every run sees the same
`tools.*`:

```bash
ferridriver session open work --extension ./gateway.ts
ferridriver run -s work --eval "return await tools.gateway.token()"
```

Passing `--extension` to `run --session` is rejected — it would silently do
nothing, since this process never loads them.

## Binding from inside a script

`browser.bind(id)` publishes a browser a script already launched, with the
same sandbox roots, caps and extensions that script has. Another terminal
can then run against it immediately:

```ts
const browser = await chromium.launch();
await browser.bind("scripted");
```

```bash
ferridriver run --session scripted --eval "return await page.snapshotForAI()"
```

## The trust boundary

A client that reaches a session's socket runs code in the host process.
The registry directory (`<cache>/ferridriver/sessions`, or
`FERRIDRIVER_SESSION_DIR`) and the socket inside it are owner-only, and
that is the whole access boundary — treat `--host`/`--port` TCP binds as an
explicit decision to widen it, not a default.

Sessions are keyed to the build that opened them: attaching with a
different ferridriver version reports a version mismatch rather than
decoding a wire it does not speak. Close and reopen the session after an
upgrade.
