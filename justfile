set shell := ["bash", "-cu"]

default: check

# Full CI check
ready: fmt lint test
  @echo "Ready to commit"

alias r := ready
alias f := fix
alias c := check

# Check compilation
check:
  cargo check --workspace --all-targets

# Run all tests (debug build)
test:
  cargo build --bin ferridriver
  cargo test --workspace
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends

# Run specific backend test (use underscores: cdp_ws, cdp_pipe, webkit)
test-backend backend:
  cargo build --bin ferridriver
  FERRIDRIVER_BIN="{{justfile_directory()}}/target/debug/ferridriver" cargo test -p ferridriver-cli --test backends -- "all_tests_{{backend}}" --nocapture

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
  cargo run --bin ferridriver -- mcp {{args}}

# Run MCP server (http)
run-http port="8080":
  cargo run --bin ferridriver -- mcp --transport http --port {{port}}

# Generate docs
doc:
  cargo doc --workspace --no-deps --open

# Clean build artifacts
clean:
  cargo clean

# Watch for changes and check
watch:
  cargo watch -x 'check --workspace'
