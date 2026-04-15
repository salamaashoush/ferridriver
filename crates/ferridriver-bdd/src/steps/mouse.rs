//! Mouse-specific step definitions: click at coordinates, move, scroll wheel, drag.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::when;

#[when("I click at position {int},{int}")]
async fn click_at_position(world: &mut BrowserWorld, x: i64, y: i64) {
  world
    .page()
    .mouse()
    .click(x as f64, y as f64, None)
    .await
    .map_err(|e| StepError::from(format!("click at ({x},{y}): {e}")))?;
}

#[when("I move mouse to {int},{int}")]
async fn move_mouse(world: &mut BrowserWorld, x: i64, y: i64) {
  world
    .page()
    .mouse()
    .r#move(x as f64, y as f64, None)
    .await
    .map_err(|e| StepError::from(format!("move mouse to ({x},{y}): {e}")))?;
}

#[when("I scroll mouse wheel down {int}")]
async fn scroll_wheel_down(world: &mut BrowserWorld, delta: i64) {
  world
    .page()
    .mouse()
    .wheel(0.0, delta as f64)
    .await
    .map_err(|e| StepError::from(format!("scroll wheel down {delta}: {e}")))?;
}

#[when("I scroll mouse wheel up {int}")]
async fn scroll_wheel_up(world: &mut BrowserWorld, delta: i64) {
  world
    .page()
    .mouse()
    .wheel(0.0, -(delta as f64))
    .await
    .map_err(|e| StepError::from(format!("scroll wheel up {delta}: {e}")))?;
}

#[when("I drag from {int},{int} to {int},{int}")]
async fn drag_coordinates(world: &mut BrowserWorld, x1: i64, y1: i64, x2: i64, y2: i64) {
  let mouse = world.page().mouse();
  mouse
    .r#move(x1 as f64, y1 as f64, None)
    .await
    .map_err(|e| StepError::from(format!("move to ({x1},{y1}): {e}")))?;
  mouse
    .down(None)
    .await
    .map_err(|e| StepError::from(format!("mouse down at ({x1},{y1}): {e}")))?;
  mouse
    .r#move(x2 as f64, y2 as f64, Some(10))
    .await
    .map_err(|e| StepError::from(format!("move to ({x2},{y2}): {e}")))?;
  mouse
    .up(None)
    .await
    .map_err(|e| StepError::from(format!("mouse up at ({x2},{y2}): {e}")))?;
}
