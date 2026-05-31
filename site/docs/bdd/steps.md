# Built-in steps (145)

Grouped by source module in
[`crates/ferridriver-bdd/src/steps/`](https://github.com/salamaashoush/ferridriver/tree/main/crates/ferridriver-bdd/src/steps).
Counts reflect actual `#[given]` / `#[when]` / `#[then]` / `#[step]`
registrations.

| Module       | Count | Coverage |
|--------------|-------|----------|
| `assertion`  | 34    | Text, visibility, value, attribute, class, state, count, role, ARIA |
| `interaction`| 20    | Click / double-click / right-click, fill, clear, type, hover, focus, blur, drag, scroll, select, check, uncheck |
| `network`    | 14    | Mock / block / intercept / remove routes, request-made assertions, fetch + response status / body / header assertions |
| `api`        | 11    | API request context: GET / POST / PUT / DELETE / PATCH, headers, body, status / JSON assertions |
| `storage`    | 8     | localStorage / sessionStorage set / remove / clear, save / load storage state |
| `wait`       | 7     | Wait for milliseconds / seconds, selector, text content, visible / hidden, retry-within-N-seconds |
| `navigation` | 6     | Navigate, back, forward, reload, URL assertions |
| `frame`      | 6     | Switch to a named frame or the main frame, frame count / existence, evaluate in the active frame |
| `dialog`     | 5     | Accept / dismiss, prompt text, assert message |
| `emulation`  | 6     | Viewport, user agent, geolocation, color scheme, timezone, locale |
| `mouse`      | 5     | Click at coordinates, move to coordinates, wheel up / down, drag between coordinates |
| `window`     | 5     | Open / switch / close tabs, tab count, bring tab to front |
| `keyboard`   | 4     | Press key, press on selector, type, press with modifier |
| `javascript` | 3     | Evaluate an expression, store its result, evaluate and assert the result |
| `cookie`     | 3     | Add, delete, clear all |
| `screenshot` | 3     | Full page, element-scoped, accessibility snapshot |
| `variable`   | 3     | Set a variable, store the text or value of a selector as a variable |
| `file`       | 2     | Attach one file or multiple files to an input |

To enumerate concrete expression strings at runtime, call
`StepRegistry::reference()` from a `bdd_main!()` binary, or pass
`--reporter usage` to see expression-level call statistics after a run.
