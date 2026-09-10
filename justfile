set shell := ["bash", "-cu"]

default: check

# Set up git hooks (run once after cloning)
setup:
  git config core.hooksPath .githooks
  @echo "Git hooks configured"

# Full CI check
ready *args:
  cargo gate ready {{args}}
  @echo "Ready to commit"

alias r := ready
alias f := fix
alias c := check
alias t := test
alias tf := test-fast

# Run both acceptance suites with the gate's build and worker budget.
acceptance *args:
  cargo gate test --only acceptance --only acceptance-bdd {{args}}

check:
  cargo check --locked --workspace --all-targets

test *args:
  cargo gate test {{args}}

# The Rust, e2e and BDD half of `just test`.
test-rust:
  cargo gate test --only rust-build --only doc-tests --only e2e --only bdd

# The Node addon has its own build dependency in the gate.
test-node *args:
  cargo gate test --only napi --only types {{args}}

test-types:
  cargo gate test --only types

test-fast *args:
  cargo gate test {{args}}

test-integration *args:
  cargo build --locked --bin ferridriver --bin ferridriver-fixtures --bin ferridriver-runtime-probe --bin ferridriver-gate --bin sidecar_echo
  ./target/debug/ferridriver test --no-inherit --headless --config tests/integration/ferridriver.toml {{args}}

# Run one backend: the native e2e project and MCP tests.
# Accepts either naming (cdp-pipe/cdp_pipe, cdp-raw/cdp_raw, bidi, webkit).
test-backend backend:
  #!/usr/bin/env bash
  set -euo pipefail
  cargo build --locked --bin ferridriver --bin ferridriver-fixtures
  project="$(echo "{{backend}}" | tr '_' '-')"
  ./target/debug/ferridriver test --headless --project "$project"
  ./target/debug/ferridriver test 'tests/integration/mcp-*.test.mjs' --no-inherit --headless --config tests/integration/ferridriver.toml --grep "${project}:"

# Run a script/test target with the QuickJS leak dump on.
#
# QuickJS lists every object still alive when the runtime is freed, which
# is how you find a native closure that captured a JS value: the
# collector cannot see such a cycle, so the objects simply survive
# teardown (and eventually trip an assertion). Pass a test filter, e.g.
#   just leak-check fetch_body_init
# Debug aid only — the flag is compiled into the QuickJS runtime.
leak-check *args:
  cargo test -p ferridriver-script --features js-dump-leaks {{args}} -- --nocapture --test-threads=1

# Lint. `--workspace` so this is the same set CI lints: without it
# clippy runs over default-members, which excludes ferridriver-node, and
# a NAPI binding can then fail CI having passed locally.
lint:
  cargo clippy --workspace --all-targets -- -D warnings

# Format check
fmt:
  cargo fmt --all -- --check

# Format fix
fmt-fix:
  cargo fmt --all

# Fix lint + format, over the same set `lint` checks.
fix: fmt-fix
  cargo clippy --workspace --all-targets --fix --allow-dirty

# Build release
build:
  cargo build --release --bin ferridriver

# Build fast release (thin LTO, parallel codegen)
build-fast:
  cargo build --profile release-fast --bin ferridriver

# Run MCP server (stdio)
run *args:
  cargo run --bin ferridriver -- {{args}}

# Run MCP server (http)
run-http port="8080":
  cargo run --bin ferridriver -- --transport http --port {{port}}

# Run the TS e2e suite through the native runner (all projects; pass --project <p> to narrow)
test-e2e *args:
  cargo build --locked --bin ferridriver --bin ferridriver-fixtures
  ./target/debug/ferridriver test --headless {{args}}

# Check ferridriver-perf against the engine it is a port of.
#
# `ferridriver-perf` is a port of devtools-frontend's trace analysis, so
# its own tests can only show it does what its author thought. This runs
# the real engine (the prebuilt devtools-frontend bundle that ships
# inside chrome-devtools-mcp) over the same traces, diffs its verdicts
# against the recordings checked in beside them, and then runs the
# comparison. Needs node and one npm install.
#
# The offline half is `cargo test -p ferridriver-perf --test
# differential`, which reads those recordings and runs in `just test`.
# This recipe is what proves the recordings are still what the engine
# says. One insight differs on purpose; scripts/perf-diff/README.md says
# which and why.
perf-diff:
  #!/usr/bin/env bash
  set -euo pipefail
  root="{{justfile_directory()}}"
  cd "$root/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  stale=0
  for trace in "$root"/crates/ferridriver-perf/tests/fixtures/*.json.gz; do
    name="$(basename "$trace" .json.gz)"
    echo "=== $name"
    node engine.mjs "$trace" > "$work/$name.json"
    if ! diff -u "$root/crates/ferridriver-perf/tests/fixtures/$name.upstream.json" "$work/$name.json"; then
      echo "  recording is out of date with the engine -- run: just perf-diff-update" >&2
      stale=1
    fi
  done
  [ "$stale" -eq 0 ]
  cd "$root" && cargo test -p ferridriver-perf --test differential

# Re-record what the real engine says about each fixture trace.
#
# Run after deliberately changing what we compare, or after bumping the
# chrome-devtools-mcp pin in scripts/perf-diff/package.json. Read the
# resulting diff before committing it: every line of it is a change in
# what we are being measured against.
perf-diff-update:
  #!/usr/bin/env bash
  set -euo pipefail
  root="{{justfile_directory()}}"
  cd "$root/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  for trace in "$root"/crates/ferridriver-perf/tests/fixtures/*.json.gz; do
    name="$(basename "$trace" .json.gz)"
    node engine.mjs "$trace" > "$root/crates/ferridriver-perf/tests/fixtures/$name.upstream.json"
  done
  cd "$root" && git diff --stat -- crates/ferridriver-perf/tests/fixtures

# What Lighthouse's own audits conclude about a page.
#
# The trace differential cannot reach these: Lighthouse audits read
# artifacts gathered from a live DOM, not a saved trace, so this drives a
# real page instead. Snapshot mode, so a static fixture answers the same
# way every run and the comparison is a gate rather than a race.
#
# Serve the fixtures first (`python3 scripts/perf-diff/fixture-server.py`)
# and pass a page, e.g. `just lh-audit http://127.0.0.1:8732/rich/`.
# CHROME_PATH overrides the browser; otherwise the one `ferridriver
# install` put in the Playwright cache is used.
lh-audit url:
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  if [ -z "${CHROME_PATH:-}" ]; then
    for candidate in \
      "$HOME/Library/Caches/ms-playwright"/chromium-*/chrome-mac-*/"Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" \
      "$HOME/.cache/ms-playwright"/chromium-*/chrome-linux/chrome; do
      [ -x "$candidate" ] && export CHROME_PATH="$candidate" && break
    done
  fi
  if [ -z "${CHROME_PATH:-}" ]; then
    echo "no Chrome found; set CHROME_PATH or run: ferridriver install chromium" >&2
    exit 1
  fi
  node lighthouse.mjs "{{url}}"

# Check our accessibility audit against Lighthouse's.
#
# Both run axe-core, so where they overlap they must agree exactly. What
# they do not share is scope: Lighthouse narrows axe to the rules its own
# audits wrap, while this runs the engine and reports every rule it has.
# Extra rules on our side are the point; the comparison is over the
# intersection. A rule Lighthouse reached and we never ran is NOT, and
# fails: ours is meant to be the superset.
#
# With no argument this is the gate — it re-derives Lighthouse's verdicts
# for the fixture pages, fails if they have drifted from the recordings
# checked in beside them, and then runs the comparison. That comparison
# is `cargo test -p ferridriver-perf --test lighthouse`, which reads the
# recordings and so runs offline, in `just test`.
#
# With a URL it is the exploratory half, against any live page:
# `just a11y-diff http://127.0.0.1:8732/rich/` (serve the fixtures
# first). Nothing is recorded and nothing gates.
a11y-diff url="":
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  cd "{{justfile_directory()}}"
  if [ -n "{{url}}" ]; then
    python3 scripts/perf-diff/compare-accessibility.py "{{url}}"
  else
    python3 scripts/perf-diff/record-lighthouse.py
    cargo test -p ferridriver-perf --test lighthouse -- accessibility_
  fi

# Check our ten live-page audits against Lighthouse's own.
#
# `doctype`, `meta-description`, `canonical`, `crawlable-anchors`,
# `link-text`, `image-aspect-ratio`, `image-size-responsive`,
# `paste-preventing-inputs`, `http-status-code` and `is-crawlable`,
# ported in `ferridriver::audits`. Nothing here agrees by construction
# the way the axe comparison does: both sides are separate
# implementations of the same arithmetic, so this is the only thing
# saying the port is right.
#
# With no argument this is the gate; with a URL it is the exploratory
# half, against any live page. Same shape as `a11y-diff`.
quality-diff url="":
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  cd "{{justfile_directory()}}"
  if [ -n "{{url}}" ]; then
    python3 scripts/perf-diff/compare-page-quality.py "{{url}}"
  else
    python3 scripts/perf-diff/record-lighthouse.py
    cargo test -p ferridriver-perf --test lighthouse -- page_quality_
  fi

# Check our robots.txt parser against the package it is a port of.
#
# `is-crawlable` does not parse robots.txt itself -- Lighthouse imports
# `robots-parser`, so agreeing with Lighthouse means agreeing with that
# package, down to the parts no specification pins: which of two
# equal-length rules wins, what an empty `Disallow:` does to the `*`
# fallback, how a pattern is percent-encoded before it is matched.
#
# `scripts/perf-diff/record-robots.mjs` holds the cases and asks the
# real package; the answers are checked in, so the comparison
# (`cargo test -p ferridriver-perf --test robots`) runs offline in
# `just test`. Pass `--update` to re-record after adding a case.
robots-diff *args:
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  node record-robots.mjs {{args}}
  cd "{{justfile_directory()}}"
  cargo test -p ferridriver-perf --test robots

# Re-record what Lighthouse says about each fixture page.
#
# One recording per page serves both comparisons. Run after deliberately
# changing a fixture page, or after bumping the chrome-devtools-mcp pin
# in scripts/perf-diff/package.json. Read the resulting diff before
# committing it: every line of it is a change in what we are being
# measured against.
lh-record:
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  cd "{{justfile_directory()}}"
  python3 scripts/perf-diff/record-lighthouse.py --update
  git diff --stat -- crates/ferridriver-perf/tests/fixtures/lighthouse

# Check our heap snapshot analysis against the engine it is a port of.
#
# The engine is the real `devtools-heap-snapshot-worker.js` that ships
# inside chrome-devtools-mcp, the same one DevTools itself runs, driven
# through the proxy its own tools use. Nothing on that side is
# reimplemented, so a disagreement means the port is wrong.
#
# The SNAPSHOT is what is checked in, not the page: a heap snapshot of a
# live page differs run to run in object ids, addresses and how much of
# V8 happens to be alive, so re-capturing would make the comparison a
# race rather than a gate. `just heap-diff --capture` re-takes them,
# which is a deliberate act that changes what we are measured against;
# `--update` re-records the engine's verdicts about them.
#
# One fixture is not captured at all. A browser will not produce a
# snapshot with user roots in it, so `handmade.heapsnapshot` is built by
# `scripts/perf-diff/make-heapsnapshot.mjs`; without it the shallow-size
# transfer, the page-object marking and half the distance walk are
# branches neither side takes.
heap-diff *args:
  #!/usr/bin/env bash
  set -euo pipefail
  cd "{{justfile_directory()}}/scripts/perf-diff"
  [ -d node_modules ] || npm install --registry=https://registry.npmjs.org --no-audit --no-fund
  if [ -z "${CHROME_PATH:-}" ]; then
    for candidate in \
      "$HOME/Library/Caches/ms-playwright"/chromium-*/chrome-mac-*/"Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" \
      "$HOME/.cache/ms-playwright"/chromium-*/chrome-linux/chrome; do
      [ -x "$candidate" ] && export CHROME_PATH="$candidate" && break
    done
  fi
  node record-heap.mjs {{args}}
  cd "{{justfile_directory()}}"
  cargo test -p ferridriver-heap --test differential

# Print our own analysis of a trace, in the shape the recordings use.
perf-report trace:
  cargo run -p ferridriver-perf --example report -- {{trace}}

# Time a trace analysis end to end, and then phase by phase.
#
# `bench` says how long; `phases` says which part to go and look at.
# Release build: the debug numbers are dominated by bounds checks and
# say nothing useful about where the work is.
perf-bench trace iters="30":
  cargo run --release -p ferridriver-perf --example bench -- {{trace}} {{iters}}
  @echo ""
  cargo run --release -p ferridriver-perf --example phases -- {{trace}} {{iters}}

# Stress the MCP server: concurrent load across sessions, page/context/
# instance churn, and a browser killed behind its back. An agent session
# drives one tool at a time, so none of this is reachable in normal use —
# which is exactly why the bugs live here. Non-zero exit on any failed
# call, any browser outliving the server, or a server that had to be
# killed. `--backend cdp-raw|webkit|bidi` to switch protocol.
stress *args:
  cargo build --bin ferridriver
  python3 scripts/stress/stress.py {{args}}

# Adversarial variant: two things happening to one session at once
# (32 callers on one context, close under load), plus deliberately
# expensive calls (4000-node DOM, 3000-line console storm). Looks for a
# wedged server rather than a slow one.
stress-adversarial *args:
  cargo build --bin ferridriver
  python3 scripts/stress/adversarial.py {{args}}

# Soak: N session lifecycles through one server. fds, threads and temp
# dirs must stay FLAT; resident memory should flatten after the early
# climb. Default 1000 cycles, roughly a minute.
stress-soak *args:
  cargo build --bin ferridriver
  python3 scripts/stress/soak.py {{args}}

# Does a browser ever outlive its server? Ends the server by stdin EOF,
# SIGTERM and SIGKILL in turn and counts what is left. Expect zero
# survivors on every backend.
stress-orphans *args:
  cargo build --bin ferridriver
  python3 scripts/stress/orphans.py {{args}}

# Every stress harness across every backend. Minutes, not seconds.
stress-all:
  #!/usr/bin/env bash
  set -euo pipefail
  cargo build --bin ferridriver
  for backend in cdp-pipe cdp-raw webkit bidi; do
    echo "=== $backend ==="
    python3 scripts/stress/orphans.py --backend "$backend"
    python3 scripts/stress/adversarial.py --backend "$backend"
  done
  python3 scripts/stress/stress.py --rounds 20 --workers 12
  python3 scripts/stress/soak.py --cycles 1000

# Run Playwright's own example suites, UNMODIFIED, against ferridriver.
# Every failure is a compat bug until docs/playwright-compat.md records it
# as an intentional divergence. `--offline` skips the network-backed
# examples; `--example <name>` narrows.
compat *args:
  cargo build --bin ferridriver
  ./scripts/playwright-compat.sh {{args}}

# Re-record the compat pass/fail baseline (after fixing a gap) or the
# corpus checksum manifest (after pulling upstream).
compat-update *args:
  cargo build --bin ferridriver
  ./scripts/playwright-compat.sh --update-baseline {{args}}

# Whole-suite A/B against Playwright Test on the SAME specs, same
# Chromium, same worker count. Read BENCHMARKING.md before citing a
# number from it. Usage: just bench-vs-playwright <spec-dir> [runs] [workers]
bench-vs-playwright spec_dir runs="5" workers="4":
  cargo build --profile release-fast --bin ferridriver
  ./scripts/bench-vs-playwright.sh {{spec_dir}} {{runs}} {{workers}}

alias bdd := test-bdd

# Build CLI then run BDD feature tests
test-bdd *args:
  cargo build --locked --bin ferridriver --bin ferridriver-fixtures
  ./target/debug/ferridriver bdd --headless {{args}} tests/features/

# Bump version everywhere, commit, tag, and push to trigger release CI.
# Usage: just release 0.3.0
release version:
  #!/usr/bin/env bash
  set -euo pipefail
  VERSION="{{version}}"
  if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Usage: just release X.Y.Z" >&2; exit 1
  fi
  echo "Bumping to $VERSION..."
  # Rust: workspace version + workspace dependency versions.
  # -i.bak (no space) is the portable in-place form across GNU and BSD sed.
  sed -i.bak "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
  sed -i.bak "s/\(ferridriver[a-z-]* = { path = \"[^\"]*\", version = \)\"[^\"]*\"/\1\"$VERSION\"/" Cargo.toml
  rm -f Cargo.toml.bak
  cargo generate-lockfile 2>/dev/null || true
  # npm: the @ferridriver/node package.json -- replace the "version" field
  f=crates/ferridriver-node/package.json
  node -e "const f='$f';const p=JSON.parse(require('fs').readFileSync(f));p.version='$VERSION';require('fs').writeFileSync(f,JSON.stringify(p,null,2)+'\n')"
  # Verify
  echo "Rust:  $(grep '^version' Cargo.toml)"
  echo "NAPI:  $(grep '\"version\"' crates/ferridriver-node/package.json | head -1 | xargs)"
  # Commit, tag, push
  git add -A
  git commit -m "release: v$VERSION"
  git tag "v$VERSION"
  git push && git push --tags
  echo ""
  echo "Pushed v$VERSION -- release CI triggered."

# Re-trigger a failed release by deleting and re-pushing the tag.
# Usage: just release-retry 0.3.0
release-retry version:
  #!/usr/bin/env bash
  set -euo pipefail
  VERSION="{{version}}"
  TAG="v$VERSION"
  echo "Deleting tag $TAG (local + remote)..."
  git tag -d "$TAG" 2>/dev/null || true
  git push origin ":refs/tags/$TAG" 2>/dev/null || true
  # Also delete the draft GitHub release if it exists
  gh release delete "$TAG" --yes 2>/dev/null || true
  echo "Re-tagging $TAG at HEAD..."
  git tag "$TAG"
  git push --tags
  echo ""
  echo "Re-pushed $TAG -- release CI re-triggered."

# Generate rustdoc
doc:
  cargo doc --workspace --no-deps --open

# Run the docs site dev server
docs:
  cd site && bun run dev

# Build the static docs site (output: site/doc_build)
docs-build:
  cd site && bun run build

# Preview the built static docs site
docs-preview:
  cd site && bun run preview

# Clean build artifacts
clean:
  cargo clean

# Watch for changes and check
watch:
  cargo watch -x 'check --workspace'

# ── Profiling ──────────────────────────────────────────────────────────────

# Run deep profile: microsecond breakdown + chrome trace timeline
profile:
  FERRIDRIVER_PROFILE=chrome RUST_LOG=info \
    cargo test --profile release-fast -p ferridriver-test --test bench_profile deep_profile \
    --features ferridriver-test/profiling -- --ignored --nocapture
  @echo ""
  @echo "Chrome trace written to trace-*.json"
  @echo "Open in: chrome://tracing  or  ui.perfetto.dev"

# Install profiling tools (one-time)
profile-setup:
  cargo install samply tokio-console

# CPU flame graph with samply (4 parallel workers, 80 test cycles)
profile-cpu:
  cargo build --profile release-fast -p ferridriver-test --bin bench-profile
  samply record -- ./target/release-fast/bench-profile

# Chrome trace only (parallel bench, no timing report)
profile-trace:
  FERRIDRIVER_PROFILE=chrome RUST_LOG=info \
    cargo run --profile release-fast -p ferridriver-test --bin bench-profile --features ferridriver-test/profiling
  @echo "Open chrome://tracing or ui.perfetto.dev and load trace-*.json"

# CPU flame graph single-threaded
profile-cpu-single:
  cargo build --profile release-fast -p ferridriver-test --bin bench-single
  samply record -- ./target/release-fast/bench-single

# Chrome trace single-threaded
profile-trace-single:
  FERRIDRIVER_PROFILE=chrome RUST_LOG=info \
    cargo run --profile release-fast -p ferridriver-test --bin bench-single --features ferridriver-test/profiling
  @echo "Open chrome://tracing or ui.perfetto.dev and load trace-*.json"

# tokio-console live async runtime dashboard
profile-console:
  FERRIDRIVER_PROFILE=console \
    cargo run --profile release-fast -p ferridriver-test --bin bench-profile --features ferridriver-test/tokio-console
