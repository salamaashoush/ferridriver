set shell := ["bash", "-cu"]

default: check

# Set up git hooks (run once after cloning)
setup:
  git config core.hooksPath .githooks
  @echo "Git hooks configured"

# Full CI check
ready: fmt lint test
  @echo "Ready to commit"

alias r := ready
alias f := fix
alias c := check
alias t := test
alias tf := test-fast

# Check compilation
check:
  cargo check --workspace --all-targets

# Run all tests: build binary + NAPI, all Rust crates (incl. all backends), TS tests, BDD features
test:
  cargo build --bin ferridriver
  cd crates/ferridriver-node && bun run build:debug
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test --workspace
  cd crates/ferridriver-node && bun test
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" bun run packages/ferridriver-test/src/cli.ts test

# Run all tests with maximum parallelism
test-fast:
  cargo build --bin ferridriver
  cd crates/ferridriver-node && bun run build:debug
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test --workspace --exclude ferridriver-cli & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_cdp_pipe" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_cdp_raw" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_bidi" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_webkit" & \
  cd crates/ferridriver-node && bun test & \
  wait
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" bun run packages/ferridriver-test/src/cli.ts test

# Run specific backend test (use underscores: cdp_ws, cdp_pipe, webkit, bidi)
test-backend backend:
  cargo build --bin ferridriver
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_{{backend}}" --nocapture

# Run NAPI/TypeScript tests with parallel backends
test-ts:
  cd crates/ferridriver-node && bun test

# Run all NAPI tests per backend in parallel processes
test-ts-fast:
  cd crates/ferridriver-node && \
  FERRIDRIVER_BACKEND=cdp-pipe bun test & \
  FERRIDRIVER_BACKEND=cdp-raw bun test & \
  wait

# Lint
lint:
  cargo clippy --workspace --all-targets -- -D warnings

# Format check
fmt:
  cargo fmt --all -- --check

# Format fix
fmt-fix:
  cargo fmt --all

# Fix lint + format
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

# Run BDD feature tests (via TS CLI)
bdd *args:
  bun run packages/ferridriver-test/src/cli.ts test {{args}} tests/features/

# Build + run BDD feature tests (via TS CLI)
test-bdd *args:
  bun run packages/ferridriver-test/src/cli.ts test {{args}} tests/features/

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
  # Rust: workspace version (single source of truth for all crates)
  sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
  cargo generate-lockfile 2>/dev/null || true
  # npm: all package.json files -- replace any version string on a "version" line
  for f in crates/ferridriver-node/package.json \
           packages/ferridriver-test/package.json \
           packages/ct-core/package.json \
           packages/ct-react/package.json \
           packages/ct-solid/package.json \
           packages/ct-svelte/package.json \
           packages/ct-vue/package.json; do
    # Use node for reliable cross-platform JSON version update
    node -e "const f='$f';const p=JSON.parse(require('fs').readFileSync(f));p.version='$VERSION';require('fs').writeFileSync(f,JSON.stringify(p,null,2)+'\n')"
  done
  # TS CLI hardcoded version
  sed -i '' "s/version: '[^']*'/version: '$VERSION'/" packages/ferridriver-test/src/cli.ts
  # Verify
  echo "Rust:  $(grep '^version' Cargo.toml)"
  echo "NAPI:  $(grep '\"version\"' crates/ferridriver-node/package.json | head -1 | xargs)"
  echo "Test:  $(grep '\"version\"' packages/ferridriver-test/package.json | head -1 | xargs)"
  echo "CLI:   $(grep "version:" packages/ferridriver-test/src/cli.ts | head -1 | xargs)"
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

# Generate docs
doc:
  cargo doc --workspace --no-deps --open

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

# Profile the TS CLI pipeline (file discovery, import, registration, execution)
profile-ts *args:
  cd packages/ferridriver-test && FERRIDRIVER_PROFILE=cli bun run src/cli.ts test {{args}}

# tokio-console live async runtime dashboard
profile-console:
  FERRIDRIVER_PROFILE=console \
    cargo run --profile release-fast -p ferridriver-test --bin bench-profile --features ferridriver-test/tokio-console
