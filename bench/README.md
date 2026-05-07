# bench/

Real-app head-to-head: ferridriver vs Playwright. Two bench tracks
that share the same `bench/app/` workload:

1. **spec.ts bench** (`run.sh`) — TS test API, 1000 spec.ts tests via
   `@ferridriver/test` vs `@playwright/test`. The original bench;
   exercises the test runner, locator engine, auto-wait, expect.
2. **BDD bench** (`run-bdd.sh`) — Gherkin `.feature` files run via
   `ferridriver bdd` vs `playwright-bdd`. Same grammar both sides.

## Layout

```
bench/
  app/                # Kitchen-sink React app (Vite + Tailwind + react-query + react-router)
                      # + Bun.serve mock API on /api/*

  # spec.ts bench
  fd-tests/           # 1000 ferridriver tests via @ferridriver/test
  pw-tests/           # mirror via @playwright/test
  run.sh              # orchestrator (workers x chrome x runs matrix)

  # BDD bench
  bdd-features/
    generate.sh           # Emits Scenario Outline + Examples for each workload
    generated/*.feature   # todos, forms-{valid,invalid}, blog-{list,detail}, dashboard, wizard
  fd-bdd/
    ferridriver.toml      # points at ../bdd-features/generated
  pw-bdd/
    playwright.config.ts  # defineBddConfig + featuresRoot=../bdd-features
    steps/steps.ts        # Given/When/Then mirroring ferridriver-bdd grammar
  run-bdd.sh              # orchestrator (workers x chrome x runs matrix)

  results/                # bench output (realapp.txt, bdd.txt)
  _legacy/                # archived prior iterations
```

## Run

spec.ts bench:

```bash
bench/run.sh                # full matrix (2/4/8 workers x chrome modes x 2 runs)
bench/run.sh smoke          # 4w x 1 run (sanity, ~30s)
bench/run.sh fd-only        # ferridriver only
bench/run.sh pw-only        # Playwright only
bench/run.sh scaling        # 1/2/4/8 workers x 1 run
```

BDD bench:

```bash
bench/run-bdd.sh            # full matrix (2/4/8 workers x chrome modes x 2 runs)
bench/run-bdd.sh smoke      # 4w x 1 run
bench/run-bdd.sh scaling    # 1/2/4/8 workers x 1 run
bench/run-bdd.sh fd-only    # ferridriver bdd only
bench/run-bdd.sh pw-only    # playwright-bdd only
```

## Architecture

The app is intentionally non-trivial:

- Async data fetching via `@tanstack/react-query` with deliberate API latency.
- Real form validation with `react-hook-form + zod`.
- Multi-step wizard with state retention across step transitions.
- Combinatorial filter dashboard reading 500-row dataset.
- Routed sub-pages (`react-router-dom 7`).

Both bench tracks spawn the same `bun ../app/server.ts` via their
respective `webServer` config, wait for `/healthz`, and tear down at
end.

## Why these workloads

Each surface stresses a different real-world cost:

- **todos** (250): input fill + click + DOM query — exercises selector engine.
- **blog-list** (50): react-query async load + client-side filter.
- **blog-detail** (200): routed nested fetch (params + nested query).
- **dashboard** (200): heavy DOM (500-row table) + filter recompute + sort.
- **forms-valid** (100) + **forms-invalid** (100): form-state + zod validation + async POST.
- **wizard** (100): state retention across 4 steps with conditional fields.

1000 scenarios total — closer to a real test suite than data-URL
clickers. Exposes the cost of the runner, locator engine, auto-wait
polling, and assertion engine on real React state transitions.
