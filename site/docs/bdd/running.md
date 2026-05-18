# Running BDD

Features run through one of two paths, both driven by the single
`ferridriver` binary or `cargo test`.

## `ferridriver bdd`

The primary path. Runs Gherkin features through the core test runner with
Rust and/or JavaScript/TypeScript step bodies — no Node or Bun:

```bash
# Rust steps only
ferridriver bdd tests/features/

# With JavaScript / TypeScript step files
ferridriver bdd \
  --steps 'steps/**/*.{js,ts}' \
  --tags "@smoke and not @wip" \
  --workers 4 \
  --reporter junit:reports/junit.xml \
  tests/features/
```

JS/TS step files are bundled with rolldown, compiled to QuickJS bytecode
once, and linked per worker. `--steps` is repeatable and overrides
`[test].steps` from config.

Flags: `--steps <GLOB>` (repeatable), `--tags "<EXPR>"`, `--workers <N>`,
`--reporter <SPEC>` (repeatable), `--strict`, `--dry-run`,
`--order defined|random[:SEED]`, `--language <LANG>` (Gherkin keyword
language), plus the shared browser flags.

## Rust / `cargo test`

Use `bdd_main!()` and the shared runner flags:

```bash
cargo test --test bdd -- --headed -j 4 --tags "@smoke and not @wip"
```

Register the test binary so `harness = false` lets `bdd_main!()` own the entry point:

```toml
# Cargo.toml
[[test]]
name = "bdd"
path = "tests/bdd.rs"
harness = false

[dev-dependencies]
ferridriver-bdd = "0.1"
ferridriver-test = "0.1"
```

## Environment variables

- `FERRIDRIVER_FEATURES` — comma-separated glob patterns (default: `features/**/*.feature`)
- `FERRIDRIVER_TAGS` — tag filter expression
