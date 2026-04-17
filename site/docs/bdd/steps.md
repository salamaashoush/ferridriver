# Built-in steps (144)

Grouped by source module in [`crates/ferridriver-bdd/src/steps/`](https://github.com/salamaashoush/ferridriver/tree/main/crates/ferridriver-bdd/src/steps). Counts reflect the actual number of `#[given]` / `#[when]` / `#[then]` / `#[step]` attributes registered.

| Module | Count | Coverage |
|---|---|---|
| `assertion` | 34 | text, visibility, value, attribute, class, state, count, role, aria |
| `interaction` | 20 | click / double-click / right-click, fill, clear, type, hover, focus, blur, drag, scroll, select, check / uncheck |
| `network` | 14 | route, fulfill, continue, abort, request / response waits, HAR capture |
| `api` | 11 | API request context: GET / POST / PUT / DELETE / PATCH, headers, body, status / JSON assertions |
| `storage` | 8 | localStorage / sessionStorage get / set / clear / remove |
| `wait` | 7 | wait for selector / text / navigation / seconds / load state |
| `navigation` | 6 | navigate, back, forward, reload, URL assertions |
| `frame` | 6 | switch to frame by name / index, main frame, frame element queries |
| `dialog` | 5 | accept / dismiss, provide prompt text, assert message |
| `emulation` | 5 | viewport, user agent, geolocation, color scheme, network conditions |
| `mouse` | 5 | move to coordinates, scroll by delta, wheel events, button holds |
| `window` | 5 | window size, maximize / minimize, tab / window switching |
| `keyboard` | 4 | press key, press on selector, repeat N times, type slowly |
| `javascript` | 3 | execute, evaluate, inject script |
| `cookie` | 3 | add, delete, clear all |
| `screenshot` | 3 | full page, named file, element-scoped |
| `variable` | 3 | set variable, store text / attribute / property / count of selector as variable |
| `file` | 2 | upload to input, assert download |

To enumerate the concrete expression strings at runtime, call the [MCP server's `list_steps` tool](/mcp/tools).
