//! Trace-mode re-export. Per-test traces are recorded by the core
//! library recorder (`ferridriver::trace`, Playwright format VERSION 8):
//! the worker starts `context.tracing` when the test's context
//! materializes, `TestInfo` mirrors step boundaries into it as action
//! events, and the worker stops it (export or discard per mode) before
//! the context closes.

pub use ferridriver_config::test::TraceMode;
