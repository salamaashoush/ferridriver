# Tools

All tools accept an optional `session` parameter (default: `"default"`). Different sessions have isolated cookies, localStorage, and network state.

```
session: "admin"        isolated context named "admin"
session: "staging:qa"   context "qa" on Chrome instance "staging"
```

## Navigation (3)

- **connect** — attach to a running Chrome (debugger URL or `auto_discover`)
- **navigate** — go to URL
- **page** — manage pages / sessions (`back`, `forward`, `reload`, `new`, `close`, `select`, `list`, `close_browser`)

## Interaction (11)

`click`, `click_at`, `hover`, `fill`, `fill_form`, `type_text`, `press_key`, `drag`, `scroll`, `select_option`, `upload_file`

## Content (7)

- **snapshot** — accessibility tree with depth limiting and incremental tracking
- **screenshot** — PNG / JPEG / WebP
- **evaluate** — run JavaScript on the page
- **wait_for** — wait for selector or text
- **search_page** — grep-like text search with context
- **find_elements** — list elements matching a CSS or rich selector
- **get_markdown** — extract page as clean markdown

## State (4)

- **cookies** — get / set / delete / clear
- **storage** — localStorage get / set / list / clear
- **emulate** — viewport, user agent, geolocation, network conditions
- **diagnostics** — console, network, performance tracing

## BDD (3)

- **list_steps** — enumerate registered step definitions
- **run_step** — execute a single step
- **run_scenario** — run a whole scenario from Gherkin text

## Accessibility snapshots

`snapshot` returns an LLM-optimized accessibility tree with `[ref=eN]` identifiers. Use these refs with `click` / `hover` / `fill` for precise element targeting.

```
### Page
- URL: https://example.com
- Title: Example

### Snapshot
- heading "Example Domain" [ref=e1] [level=1]
- paragraph "This domain is for..." [ref=e2]
- link "More information..." [ref=e3] [url=https://www.iana.org/...]
```
