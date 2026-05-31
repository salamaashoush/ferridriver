# CI on GitHub Actions

A sane starting point: sharded matrix, browser cache, JUnit upload,
artifacts on failure.

## Minimal workflow

```yaml
# .github/workflows/e2e.yml
name: e2e

on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        shard: [1/4, 2/4, 3/4, 4/4]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ${{ runner.os }}-cargo-${{ hashFiles('Cargo.lock') }}

      - name: Install ferridriver
        run: cargo install ferridriver-cli --locked

      - name: Install browsers
        run: ferridriver install --with-deps chromium

      - name: Run tests
        run: |
          cargo test --test e2e --release -- \
            --shard ${{ matrix.shard }} \
            --retries 1

      - name: Upload JUnit
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: junit-${{ strategy.job-index }}
          path: test-results/junit.xml

      - name: Upload failure artifacts
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: artifacts-${{ strategy.job-index }}
          path: |
            test-results/**/*.png
            test-results/**/*.webm
            test-results/**/trace.zip
          retention-days: 7
```

The `cargo test --test e2e` harness parses `--shard`, `--project`, and
`--retries`, but it does NOT take `--reporter`. For that path the JUnit
output above comes from the config file, not a CLI flag — add a reporter
to `ferridriver.toml`:

```toml
[[test.reporter]]
name = "junit"

[[test.reporter]]
name = "terminal"
```

(The `ferridriver bdd` runner is the one that accepts `--reporter <name>`
on the command line, as shown in the BDD-only workflow below.)

## Cross-browser matrix

```yaml
strategy:
  fail-fast: false
  matrix:
    project: [chromium, firefox, webkit]
    shard:   [1/2, 2/2]
steps:
  # ... setup steps
  - name: Install browsers
    run: |
      ferridriver install --with-deps chromium firefox
      npx playwright install webkit   # for the webkit backend

  - name: Run tests
    run: |
      cargo test --test e2e --release -- \
        --project ${{ matrix.project }} \
        --shard ${{ matrix.shard }}
```

## Browser cache between runs

```yaml
- name: Cache browsers
  uses: actions/cache@v4
  with:
    path: ~/.cache/ferridriver
    key: ${{ runner.os }}-ferridriver-browsers-v1
```

Hits halve clean-build time on PRs.

## BDD-only workflow

```yaml
- name: Install ferridriver
  run: cargo install ferridriver-cli --locked

- name: Install browsers
  run: ferridriver install --with-deps chromium

- name: Run features
  run: |
    ferridriver bdd \
      --workers 4 \
      --reporter terminal \
      --reporter junit \
      --tags 'not @wip' \
      tests/features/
```

## GitHub Annotations

Add the `github` reporter to get test failures inline as annotations on
the PR:

```bash
--reporter github --reporter junit
```

## Test reports in the PR

Combine with [`dorny/test-reporter`](https://github.com/dorny/test-reporter):

```yaml
- name: Publish report
  if: always()
  uses: dorny/test-reporter@v1
  with:
    name: e2e (${{ matrix.shard }})
    path: test-results/junit.xml
    reporter: java-junit
```

## MCP server in CI

For agent-driven flows running in CI:

```yaml
- name: Start MCP server
  run: |
    ferridriver mcp --transport http --port 8080 &
    timeout 30 bash -c 'until curl -s http://localhost:8080/mcp; do sleep 1; done'

- name: Run agent flow
  run: bun run scripts/agent-eval.ts
```
