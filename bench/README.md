# bench/

Real-app head-to-head benchmark: ferridriver vs Playwright.

## Layout

```
bench/
  app/                # Kitchen-sink React app (Vite + Tailwind + react-query + react-router)
                      # + Bun.serve mock API on /api/*
  fd-tests/           # 1000 ferridriver tests
    tests/
      todos.spec.ts       (250)
      blog.spec.ts        (250 — list + detail + react-query async)
      dashboard.spec.ts   (200 — multi-filter combinatorial)
      forms.spec.ts       (200 — validation + async submit)
      wizard.spec.ts      (100 — multi-step state retention)
    ferridriver.config.ts
  pw-tests/           # 1000 Playwright tests (mirrors fd-tests; only the
                      # `import { test, expect }` source differs)
    tests/...
    playwright.config.ts
  results/            # bench output (results.txt + per-run csv)
  run.sh              # orchestrator
  _legacy/            # archived from prior bench iterations
```

## Run

```bash
bench/run.sh                # full matrix (1/4/8/16 workers × headless+regular Chrome × 3 runs)
bench/run.sh smoke          # 4w × 1 run (sanity, ~30s)
bench/run.sh fd-only        # ferridriver only
bench/run.sh pw-only        # Playwright only
```

## Architecture

**The app** is intentionally non-trivial:
- Async data fetching via `@tanstack/react-query` with deliberate API latency.
- Real form validation with `react-hook-form + zod`.
- Multi-step wizard with state retention across step transitions.
- Combinatorial filter dashboard reading 500-row dataset.
- Routed sub-pages (`react-router-dom 7`).

Both runners spawn the same `bun ../app/server.ts` via their respective
`webServer` config option, wait for `/healthz`, and tear down at end.

**Identical test logic** in fd-tests/ and pw-tests/ — only the
`import { test, expect } from '@ferridriver/test'` vs
`from '@playwright/test'` line differs. Tests are mirrored at copy
time; if you edit one tree, mirror to the other (or use `sed` from
the orchestrator).

## Why these tests

Each surface stresses a different real-world cost:
- **todos**: input fill + click + DOM query, exercises selector engine.
- **blog list**: react-query async load + client-side filter + pagination.
- **blog detail**: routed nested fetch (params + nested query).
- **dashboard**: heavy DOM (500-row table) + filter recompute + sort.
- **forms**: form-state + zod validation + async POST round-trip.
- **wizard**: state retention across 4 steps with conditional fields.

This is closer to a real test suite than data-URL clickers — exposes
the actual cost of the runner, the locator engine, the auto-wait
polling, and the assertion engine on real React state transitions.
