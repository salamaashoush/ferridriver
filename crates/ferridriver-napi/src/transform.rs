//! TypeScript → JavaScript transpilation via oxc.
//!
//! Strips types using oxc's parser + codegen. No transformer needed —
//! the parser naturally drops type annotations when producing the AST,
//! and codegen emits clean JavaScript.
//!
//! Used by the ESM loader hook (`loader.mjs`) to enable `import`ing `.ts`
//! files in Node.js without tsx, ts-node, or any external tooling.

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Transform TypeScript source code to JavaScript by stripping types.
///
/// Uses oxc parser + codegen for maximum speed (~40x faster than tsc).
/// Handles `.ts`, `.tsx`, `.mts`, `.cts` files based on the filename extension.
#[napi]
pub fn transform_typescript(code: String, filename: String) -> Result<String> {
  let allocator = oxc_allocator::Allocator::default();
  let source_type = oxc_span::SourceType::from_path(&filename).unwrap_or_else(|_| {
    // Default to TSX if we can't determine from extension.
    oxc_span::SourceType::tsx()
  });
  let ret = oxc_parser::Parser::new(&allocator, &code, source_type).parse();
  if ret.panicked {
    return Err(Error::from_reason(format!(
      "failed to parse {filename}: parser panicked"
    )));
  }
  if !ret.errors.is_empty() {
    let messages: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
    return Err(Error::from_reason(format!(
      "parse errors in {filename}:\n{}",
      messages.join("\n")
    )));
  }
  Ok(oxc_codegen::Codegen::new().build(&ret.program).code)
}
