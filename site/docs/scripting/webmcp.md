# WebMCP

[WebMCP](https://github.com/webmachinelearning/webmcp) lets a page hand
tools to agents: functions it registers with
`document.modelContext.registerTool()`, or forms it marks with
`<form toolname>`. Each tool has a name, a description, an input schema
and hints about what it does. ferridriver lists and calls them from
scripts, the test runner, Node, Rust and the MCP server, through one
API: `page.webmcp` and `frame.webmcp`.

## Turning it on

WebMCP is still behind a flag in browsers.

- **Chromium**: launch with `--enable-features=WebMCP`. Some Chromium
  builds keep listing and the DevTools calls behind their own features,
  `WebMCPTesting` and `DevToolsWebMCPSupport`; ferridriver adds both to
  the same switch so `WebMCP` alone is enough. Chromium reads only the
  last `--enable-features` it is given, so put `WebMCP` in that one.
- **Firefox**: set the preferences `dom.modelcontext.enabled` and
  `dom.modelcontext.testing.enabled`.

```js
const browser = await chromium().launch({ args: ['--enable-features=WebMCP'] });
```

```toml
# The MCP server's browser
[mcp.browser]
chromeArgs = ["--enable-features=WebMCP"]
```

The API needs a secure context: an `https:` page, or `http://localhost`
and `http://127.0.0.1`. A document without WebMCP rejects with
`WebMCP discovery is unavailable in this document`.

## Listing tools

```js
const tools = await page.webmcp.tools();
// [{
//   name: 'search_flights',
//   title: 'Search flights',
//   description: 'Find flights between two airports',
//   inputSchema: { type: 'object', properties: { from: {...}, to: {...} } },
//   annotations: { readOnly: true },
//   origin: 'https://example.com',
//   frame: { name: '', url: 'https://example.com/' },
// }]
```

| Field | Meaning |
|---|---|
| `name`, `description` | As registered. |
| `title` | A human-readable label, when the page gave one. |
| `inputSchema` | The JSON Schema the input must match. A form tool's schema is built from its fields. |
| `annotations` | The hints that are set: `readOnly`, `untrustedContent` (its output may carry third-party content), `consequential` (it acts with consequences, such as a payment), `debugging`, and `autosubmit` for a form tool that submits itself. |
| `origin` | The origin of the document that registered it. |
| `declarative` | `true` for a `<form toolname>` tool. |
| `frame` | The frame that registered it, by name and URL, as it was when listed. |

`page.webmcp` covers the main frame, and `frame.webmcp` covers that frame
only. Pass `allFrames: true` to include every descendant frame,
cross-origin ones too:

```js
const everywhere = await page.webmcp.tools({ allFrames: true });
const checkout = page.frame('checkout');
await checkout.webmcp.tools();
```

## Calling a tool

```js
const result = await page.webmcp.callTool('search_flights', { from: 'CAI', to: 'AMS' });
```

The page runs the tool itself. It may navigate, change state or wait for
the user: a form tool without `toolautosubmit` fills its fields and then
waits for a real submit. The result is whatever the tool returned,
including `{ isError: true }`, which is a result rather than a failure.

A call rejects when:

- no tool has that name. The error lists the tools the frame does have;
- the tool throws. The error carries the tool's own message, `WebMCP tool
  "x" failed: Error: ...`, wherever the browser reports it (see below);
- the document navigates away, the frame detaches or the page closes
  while the tool runs;
- it outlives `timeout`, which defaults to the page's default timeout. `0`
  waits without limit.

A call that times out is canceled in the page, not abandoned: the
`AbortSignal` the tool received in its second argument aborts.

```js
document.modelContext.registerTool({
  name: 'export_report', description: 'Builds a large report',
  execute: async (input, { signal }) => buildReport(input, { signal }),
});
```

With `allFrames: true`, `callTool` finds the tool in any frame, provided
only one frame registers that name. Otherwise call it through the frame
that owns it.

## Waiting for a tool

Pages register tools when their code runs, and a `<form toolname>`
registers on a later task than the one that inserted it. Wait for the
tool rather than racing it:

```js
await page.webmcp.waitForTool('subscribe', { timeout: 5000 });
await page.webmcp.callTool('subscribe', { email: 'sashoush@example.com' });
```

`waitForTool` resolves with the tool once it is registered. On timeout it
reports that WebMCP is unavailable when the document never had it, and a
plain timeout otherwise.

## From the MCP server

The server exposes two tools, and lists the page's tools on its own when
it navigates.

- **`webmcp_tools`** lists the tools of every frame, with their schema,
  hints and frame.
- **`webmcp_call`** calls one by `name` with an `input` object. `frame`
  (a frame name or URL from the list) picks the frame when several
  register the same name; `timeout` cancels the call in the page.

`navigate` and the `page` actions that change the page append a
`### WebMCP tools` section to their reply when the main frame registers
any, so an agent learns a page offers tools without asking.

## Backends

| Backend | Listing | Calling | Cancellation |
|---|---|---|---|
| `cdp-pipe`, `cdp-raw` | In an isolated world, unaffected by page scripts that replace `getTools` | Through DevTools, with the tool's own error message | The tool's signal aborts |
| WebDriver BiDi or Classic to Chromium (ChromeDriver) | In the page | In the page; errors read `the invocation failed` | The tool's signal aborts |
| `bidi` (Firefox) | In a sandbox | In the page | None: Firefox gives a tool no signal |
| `webkit` | Unavailable | Unavailable | |

Firefox also accepts tools from the top-level window only, and does not
register `<form toolname>` tools.
