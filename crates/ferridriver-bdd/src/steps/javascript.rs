//! JavaScript execution step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, then, when};

#[when("I evaluate {string}")]
async fn evaluate(world: &mut BrowserWorld, expression: String) {
  world
    .page()
    .evaluate(&expression, ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .map_err(|e| StepError::from(format!("evaluate JS: {e}")))?;
}

#[step("I store the result of {string} as {string}")]
async fn store_result(world: &mut BrowserWorld, expression: String, var_name: String) {
  let result = world
    .page()
    .evaluate(&expression, ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .map_err(|e| StepError::from(format!("evaluate JS for variable: {e}")))?;

  world.set_var(var_name, result.as_string_lossy());
}

#[then("I evaluate {string} and expect {string}")]
async fn evaluate_and_expect(world: &mut BrowserWorld, expression: String, expected: String) {
  let result = world
    .page()
    .evaluate(&expression, ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .map_err(|e| StepError::from(format!("evaluate JS: {e}")))?;

  let actual = result.as_string_lossy();

  if actual != expected {
    return Err(StepError {
      message: format!("evaluate {expression}: expected {expected:?}, got {actual:?}"),
      diff: Some((expected, actual)),
      pending: false,
    });
  }
}
