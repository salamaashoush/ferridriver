//! Mouse-specific step definitions: click at coordinates, move, scroll wheel, drag.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::when;

#[when("I click at position {int},{int}")]
async fn click_at_position(world: &mut BrowserWorld, x: i64, y: i64) {
  world
    .page()
    .click_at(x as f64, y as f64)
    .await
    .map_err(|e| StepError::from(format!("click at ({x},{y}): {e}")))?;
}

#[when("I move mouse to {int},{int}")]
async fn move_mouse(world: &mut BrowserWorld, x: i64, y: i64) {
  world
    .page()
    .move_mouse(x as f64, y as f64)
    .await
    .map_err(|e| StepError::from(format!("move mouse to ({x},{y}): {e}")))?;
}

#[when("I scroll mouse wheel down {int}")]
async fn scroll_wheel_down(world: &mut BrowserWorld, delta: i64) {
  world
    .page()
    .mouse_wheel(0.0, delta as f64)
    .await
    .map_err(|e| StepError::from(format!("scroll wheel down {delta}: {e}")))?;
}

#[when("I scroll mouse wheel up {int}")]
async fn scroll_wheel_up(world: &mut BrowserWorld, delta: i64) {
  world
    .page()
    .mouse_wheel(0.0, -(delta as f64))
    .await
    .map_err(|e| StepError::from(format!("scroll wheel up {delta}: {e}")))?;
}

#[when("I drag from {int},{int} to {int},{int}")]
async fn drag_coordinates(world: &mut BrowserWorld, x1: i64, y1: i64, x2: i64, y2: i64) {
  world
    .page()
    .drag_and_drop((x1 as f64, y1 as f64), (x2 as f64, y2 as f64))
    .await
    .map_err(|e| StepError::from(format!("drag from ({x1},{y1}) to ({x2},{y2}): {e}")))?;
}
