//! BDD reporter implementations.
//!
//! All reporters implement `ferridriver_test::reporter::Reporter`.
//! The CLI instantiates them and passes to `TestRunner`.

pub mod cucumber_json;
pub mod json;
pub mod junit;
pub mod terminal;
