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

# Check compilation (default-members; ferridriver-node excluded)
check:
  cargo check --all-targets

# Run all tests: build CLI binary, run all Rust crates (incl. all backends), BDD features
test:
  cargo build --bin ferridriver
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo run --bin ferridriver -- bdd tests/features/

# Run all tests with maximum parallelism
test-fast:
  cargo build --bin ferridriver
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test --exclude ferridriver-cli & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_cdp_pipe" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_cdp_raw" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_bidi" & \
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_webkit" & \
  wait

# Run specific backend test (use underscores: cdp_ws, cdp_pipe, webkit, bidi)
test-backend backend:
  cargo build --bin ferridriver
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_{{backend}}" --nocapture

# Lint (default-members; ferridriver-node excluded)
lint:
  cargo clippy --all-targets -- -D warnings

# Format check
fmt:
  cargo fmt --all -- --check

# Format fix
fmt-fix:
  cargo fmt --all

# Fix lint + format (default-members; ferridriver-node excluded)
fix: fmt-fix
  cargo clippy --all-targets --fix --allow-dirty

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

# Run BDD feature tests via the Rust CLI
bdd *args:
  cargo run --bin ferridriver -- bdd {{args}} tests/features/

# Build CLI then run BDD feature tests
test-bdd *args:
  cargo build --bin ferridriver
  ./target/debug/ferridriver bdd {{args}} tests/features/

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
  # Rust: workspace version + workspace dependency versions
  sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
  sed -i '' "s/\(ferridriver[a-z-]* = { path = \"[^\"]*\", version = \)\"[^\"]*\"/\1\"$VERSION\"/" Cargo.toml
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
