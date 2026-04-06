//! JavaScript execution step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, when};

#[when("I evaluate {string}")]
async fn evaluate(world: &mut BrowserWorld, expression: String) {
  world
    .page()
    .evaluate(&expression)
    .await
    .map_err(|e| StepError::from(format!("evaluate JS: {e}")))?;
}

#[step("I store the result of {string} as {string}")]
async fn store_result(world: &mut BrowserWorld, expression: String, var_name: String) {
  let result = world
    .page()
    .evaluate(&expression)
    .await
    .map_err(|e| StepError::from(format!("evaluate JS for variable: {e}")))?;

  let value = match result {
    Some(serde_json::Value::String(s)) => s,
    Some(serde_json::Value::Number(n)) => n.to_string(),
    Some(serde_json::Value::Bool(b)) => b.to_string(),
    Some(serde_json::Value::Null) | None => "null".to_string(),
    Some(other) => other.to_string(),
  };

  world.set_var(var_name, value);
}
