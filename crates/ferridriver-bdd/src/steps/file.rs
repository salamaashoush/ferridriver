//! File upload step definitions.
//!
//! Uses `locator.set_input_files(&[paths])` to attach files to `<input type="file">` elements.

use crate::step::{DataTable, StepError};
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::when;

#[when("I attach file {string} to {string}")]
async fn attach_file(world: &mut BrowserWorld, file_path: String, selector: String) {
  world
    .page()
    .locator(&selector)
    .set_input_files(std::slice::from_ref(&file_path))
    .await
    .map_err(|e| StepError::from(format!("attach file \"{file_path}\" to \"{selector}\": {e}")))?;
}

#[when("I attach files to {string}")]
async fn attach_files(world: &mut BrowserWorld, selector: String, table: Option<&DataTable>) {
  let table = table.ok_or_else(|| StepError::from("attach files requires a data table of file paths"))?;

  let paths: Vec<String> = table
    .iter()
    .flat_map(|row| row.iter().cloned())
    .filter(|s| !s.is_empty())
    .collect();

  if paths.is_empty() {
    return Err(StepError::from("data table contained no file paths"));
  }

  world
    .page()
    .locator(&selector)
    .set_input_files(&paths)
    .await
    .map_err(|e| StepError::from(format!("attach files to \"{selector}\": {e}")))?;
}
