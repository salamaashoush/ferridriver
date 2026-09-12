# Agent protocol comparison

This benchmark measures one equivalent task: read 100 list items from a local
page, sum their numeric text, and return `fd-bench-4950` through persistent
MCP. Every response must contain that result. All tools launch the same
`/usr/bin/chromium`; network latency and paid providers are excluded.

The workload uses ferridriver's `run_script` and `page.evaluate`,
agent-browser's `agent_browser_eval`, and chrome-devtools-mcp's
`evaluate_script`. The latter receives `waitForStableDom: false`, appropriate
for this read-only task. Each process gets a fresh working directory and
session. Inherited ferridriver and agent-browser configuration is excluded.

## Reproduce

Inspect the primary sources before changing the comparison:

- [agent-browser](https://github.com/vercel-labs/agent-browser/tree/8c15ff9f71ae60c7e99e66afe1e2d4b9bf414fe2),
  especially `cli/src/mcp.rs` and `cli/src/native/webdriver/`.
- [chrome-devtools-mcp](https://github.com/ChromeDevTools/chrome-devtools-mcp/tree/d9a8cb6ec22aadf5cb964c5e97a8b047693046e2),
  especially `src/tools/script.ts` and `src/utils/WaitForHelper.ts`.
- Playwright source inspected at `d1ead3ecca23182f2d06d761c28e3d4edafb6595`,
  including `client/browser.ts`, `client/browserType.ts`, and
  `server/bidi/bidiBrowser.ts` under `packages/playwright-core/src/`.

The executable comparison uses npm releases agent-browser 0.37.1 and
chrome-devtools-mcp 1.9.0. Source inspection revisions are recorded separately
from those release versions. Install them in an isolated directory, build
ferridriver, then pass their executable paths:

```bash
python3 scripts/bench-agent-protocols.py \
  --agent-browser /path/to/agent-browser-linux-x64 \
  --chrome-devtools-mcp /path/to/chrome-devtools-mcp \
  --chromium /usr/bin/chromium \
  --samples 100 --output /tmp/protocol-comparison.json
```

The output file must not already exist. After five warmups, the harness runs
100 sequential requests and 100 requests in batches of four. It reports p50,
p95, p99, throughput, and process startup through first navigation and
evaluation. RSS measures only the MCP frontend process. It excludes browser
processes and agent-browser's daemon, so it cannot establish total-memory
superiority. Cold startup is one observation per invocation; repeat the
command with fresh output paths to assess variability.

## Interpretation and coverage

The initial measurements are in `protocol-comparison-2026-09-12.json`.
They establish a development baseline, not a release-performance guarantee.
Ferridriver used a debug-profile executable and the sibling ferrijs workspace
overrides recorded in `Cargo.toml`. Reproduction needs those runtime sources.

The inspected agent-browser MCP wrapper launches a CLI process per tool call
and polls its completion every 20ms. The inspected chrome-devtools-mcp helper
waits 100ms for possible navigation even when DOM-stability waiting is
disabled. Ferridriver's scripted read has neither wait. This explains much
of the difference for this task; it does not compare navigation handling,
screenshots, network inspection, or every browser action.

Capability coverage is also uneven. Ferridriver's gate exercises CDP pipe,
CDP WebSocket, Firefox BiDi, and Playwright WebKit. Its local WebDriver probe
additionally exercises ChromeDriver's HTTP-to-BiDi connection and mobile
emulation. The inspected agent-browser source includes a Classic WebDriver
client and Appium management; ferridriver still lacks Classic native-app
automation. Neither that source inspection nor the MCP timing run verifies
real device operation. A separate [Android emulator probe](android-appium-verification.md)
has since passed through Appium session creation and the existing CDP backend,
with a documented local Appium correction. No overall capability lead is claimed.

The resource fix measured separately here is BiDi cancellation: dropping a
pending command previously left one response slot allocated. The regression
now requires zero slots immediately after cancellation. Remote lifecycle
tests also require a DELETE after failed or cancelled initialization, and
the live ChromeDriver probe requires an empty session list after close.

## Rerun after target implementation

The same command passed at `7df36f6`, with results in
[`protocol-comparison-2026-09-12-final.json`](protocol-comparison-2026-09-12-final.json).
All three tools returned the expected sum for every measured request. Builds
and test runs had finished; other virtual machines remained active on this
shared Linux host.

| Tool | Warm p50 / p95 / p99 (ms) | Sequential requests/s | Cold observation (ms) |
| --- | --- | --- | --- |
| ferridriver | 0.572 / 0.749 / 1.170 | 1615.9 | 650.8 |
| agent-browser | 21.379 / 21.579 / 21.626 | 46.7 | 568.0 |
| chrome-devtools-mcp | 101.414 / 101.875 / 102.104 | 9.9 | 568.5 |

At four concurrent requests, measured throughput was 2286.7, 46.8, and 9.9
requests/s respectively. The JSON retains the corresponding latency
distributions and frontend RSS observations.

Ferridriver's initial sequential p50 was 0.552 ms; the rerun was 0.572 ms.
These observations do not establish a speedup from the target changes.
They show a warm latency advantage for this particular MCP task, while the
single cold observation favors the other tools. Average serialized response
sizes were approximately 6550, 4815, and 140 bytes respectively, so the warm
latency result does not establish a response-size advantage either.
