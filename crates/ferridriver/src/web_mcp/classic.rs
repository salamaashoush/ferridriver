use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::{SerializedArgument, SerializedValue, argument_from_serde};
use crate::{FerriError, Frame, Result};

static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

struct PendingCall {
  frame: Option<Frame>,
  id: String,
}

pub(super) async fn call(frame: &Frame, source: &str, input: SerializedArgument) -> Result<SerializedValue> {
  let id = format!(
    "{}:{}:{}",
    frame.page().inner().page_guid(),
    std::process::id(),
    NEXT_CALL.fetch_add(1, Ordering::Relaxed)
  );
  let id = serde_json::to_string(&id)?;
  let mut pending = PendingCall {
    frame: Some(frame.clone()),
    id: id.clone(),
  };
  let start = format!(
    r"input => {{
    const jobs = globalThis.__ferridriverWebMcpJobs ||= new Map();
    const job = {{done: false}};
    jobs.set({id}, job);
    const reference = new WeakRef(job);
    Promise.resolve().then(() => ({source})(input)).then(
      value => {{ const job = reference.deref(); if (job) {{ job.value = value; job.done = true; }} }},
      error => {{ const job = reference.deref(); if (job) {{ job.error = error; job.failed = true; job.done = true; }} }}
    );
    return null;
  }}"
  );
  evaluate_control(frame, &start, input).await?;
  let poll = format!(
    r"() => {{
    const jobs = globalThis.__ferridriverWebMcpJobs;
    const job = jobs?.get({id});
    if (!job) throw new Error('WebMCP document changed while the tool was running');
    if (!job.done) return [false];
    jobs.delete({id});
    if (job.failed) throw job.error;
    return [true, job.value];
  }}"
  );
  loop {
    let result = evaluate_control(frame, &poll, argument_from_serde(&())?).await?;
    match result {
      SerializedValue::Array { mut items, .. }
        if items.len() == 2 && items.first() == Some(&SerializedValue::Bool(true)) =>
      {
        pending.frame.take();
        return items
          .pop()
          .ok_or_else(|| FerriError::protocol("WebMCP", "missing tool result"));
      },
      SerializedValue::Array { items, .. } if items == [SerializedValue::Bool(false)] => {},
      _ => return Err(FerriError::protocol("WebMCP", "invalid pending tool result")),
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}

async fn evaluate_control(frame: &Frame, source: &str, input: SerializedArgument) -> Result<SerializedValue> {
  let caller = crate::operation_budget::OperationBudget::capture();
  if let Some(caller) = caller {
    caller.remaining_ms().map_err(|error| error.error("WebMCP operation"))?;
  }
  // These scripts do not await the tool. Keep their selection lease until
  // the response arrives, even when the logical tool deadline expires.
  let control = crate::operation_budget::OperationBudget::wall_clock(frame.page().default_timeout())?;
  crate::backend::webdriver::session::with_command_admission(
    caller,
    control.scope(frame.evaluate_untraced(source, input, Some(true))),
  )
  .await
}

impl Drop for PendingCall {
  fn drop(&mut self) {
    let Some(frame) = self.frame.take() else { return };
    let id = &self.id;
    let source = format!("() => {{ globalThis.__ferridriverWebMcpJobs?.delete({id}); }}");
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      runtime.spawn(async move {
        let cleanup = async {
          let budget = crate::operation_budget::OperationBudget::wall_clock(5000)?;
          budget
            .scope(budget.wait(frame.evaluate_untraced(&source, argument_from_serde(&())?, Some(true))))
            .await
            .map_err(|error| error.error("removing pending WebMCP call"))??;
          Ok::<(), FerriError>(())
        }
        .await;
        if let Err(error) = cleanup {
          tracing::debug!(%error, "WebMCP call cleanup could not reach its document");
        }
      });
    }
  }
}
