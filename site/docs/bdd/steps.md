# Built-in steps (144)

Grouped by source module in
[`crates/ferridriver-bdd/src/steps/`](https://github.com/salamaashoush/ferridriver/tree/main/crates/ferridriver-bdd/src/steps).
Counts reflect actual `#[given]` / `#[when]` / `#[then]` / `#[step]`
registrations.

| Module       | Count | Coverage |
|--------------|-------|----------|
| `assertion`  | 34    | Text, visibility, value, attribute, class, state, count, role, ARIA |
| `interaction`| 20    | Click / double-click / right-click, fill, clear, type, hover, focus, blur, drag, scroll, select, check, uncheck |
| `network`    | 14    | Route, fulfill, continue, abort, request / response waits, HAR capture |
| `api`        | 11    | API request context: GET / POST / PUT / DELETE / PATCH, headers, body, status / JSON assertions |
| `storage`    | 8     | localStorage / sessionStorage get / set / clear / remove |
| `wait`       | 7     | Wait for selector / text / navigation / seconds / load state |
| `navigation` | 6     | Navigate, back, forward, reload, URL assertions |
| `frame`      | 6     | Switch frames by name / index, main frame, frame queries |
| `dialog`     | 5     | Accept / dismiss, prompt text, assert message |
| `emulation`  | 5     | Viewport, user agent, geolocation, color scheme, network conditions |
| `mouse`      | 5     | Move to coordinates, scroll by delta, wheel events, button holds |
| `window`     | 5     | Window size, maximize / minimize, tab / window switching |
| `keyboard`   | 4     | Press key, press on selector, repeat N times, type slowly |
| `javascript` | 3     | Execute, evaluate, inject script |
| `cookie`     | 3     | Add, delete, clear all |
| `screenshot` | 3     | Full page, named file, element-scoped |
| `variable`   | 3     | Set, store text / attribute / property / count of selector as variable |
| `file`       | 2     | Upload to input, assert download |

To enumerate concrete expression strings at runtime, call
`StepRegistry::reference()` from a `bdd_main!()` binary, or pass
`--reporter usage` to see expression-level call statistics after a run.
