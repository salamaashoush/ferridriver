//! `test.step`, `TestStepInfo`, `TestInfo` object construction, attachments,
//! snapshot paths, and per-test VM world setup (`set_current_test`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rquickjs::function::Rest;
use rquickjs::prelude::Async;
use rquickjs::promise::MaybePromise;
use rquickjs::{Ctx, Function, Object, Persistent, Value};

use ferridriver_test::fixture_graph::FixtureSlot;
use ferridriver_test::host::{TestHostBridge, TestInfoData, TestWorldData};
use ferridriver_test::model::StepLocation;
use ferridriver_test::step::{StepBodyError, StepExpectation, StepFrame, StepOptions, StepSpec};

use crate::bindings::call_site;
use crate::bindings::registry::throw_script_error;
use crate::bindings::{install_browser_context_on, install_browser_on, install_page_on, install_request_on};
use crate::error::ScriptError;

use super::collect::CollectedFixture;
use super::registry::{
  AnnotationsMirror, CurrentTest, TestRegistry, current_bridge, runtime_modifier, with_test_registry,
};

// ── Invocation ───────────────────────────────────────────────────────

/// Name of the error `stepInfo.skip()` throws to unwind its own body.
/// Swallowed at the step boundary — the step is reported skipped, the
/// test carries on. Playwright's `StepSkipError`.
pub(super) const STEP_SKIP_ERROR: &str = "StepSkipError";

/// `test.step(title, body, { box, location, timeout })` and
/// `test.step.skip(...)` — opens a live reporter/trace step around the
/// body, returns the body's resolved value, re-throws its error after
/// closing the step.
///
/// Every rule (which location the step reports, what `box` re-attributes
/// to, the timeout message, when the body is skipped) is
/// `ferridriver_test::step`'s; this only lowers the JS option bag and
/// mirrors `TestStepInfo` onto a JS object.
pub(super) fn make_step_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  let step = step_callable(ctx, StepExpectation::Pass)?;
  let skip = step_callable(ctx, StepExpectation::Skip)?;
  if let Some(obj) = step.as_object() {
    obj.set("skip", skip)?;
  }
  Ok(step)
}

pub(super) fn step_callable<'js>(ctx: &Ctx<'js>, expectation: StepExpectation) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    Async(
      move |ctx: Ctx<'js>,
            frames: call_site::CallFrames,
            title: String,
            f: Function<'js>,
            options: rquickjs::function::Opt<Value<'js>>|
            -> Pin<Box<dyn Future<Output = rquickjs::Result<Value<'js>>> + 'js>> {
        // The option bag and the stack are read here, on the calling
        // stack: an `Async` body first runs when the VM executor polls
        // it, by which time the caller's frames are gone.
        let options = parse_step_options(&ctx, options.0.as_ref());
        Box::pin(async move {
          let bridge = current_bridge(&ctx, "test.step()")?;
          let spec = StepSpec::new(title)
            .with_options(options?)
            .with_frames(
              frames
                .0
                .into_iter()
                .map(|(_, line, column)| StepFrame::Host { line, column })
                .collect(),
            )
            .expecting(expectation);

          let ctx_for_body = ctx.clone();
          let bridge_for_body = Arc::clone(&bridge);
          let outcome = ferridriver_test::step::run(&*bridge, spec, move |run| async move {
            let ctx = ctx_for_body;
            let step_info = build_step_info(&ctx, &bridge_for_body, &run).map_err(|e| body_error(&ctx, e))?;
            if !run.should_run_body() {
              return Ok(Value::new_undefined(ctx.clone()));
            }
            let called: rquickjs::Result<MaybePromise<'_>> = f.call((step_info.clone(),));
            let settled = match called {
              Ok(mp) => mp.into_future::<Value<'js>>().await,
              Err(e) => Err(e),
            };
            collect_step_annotations(&step_info, &run);
            match settled {
              Ok(v) => Ok(v),
              Err(e) => {
                let failure = body_error(&ctx, e);
                if failure.name == STEP_SKIP_ERROR {
                  return Ok(Value::new_undefined(ctx.clone()));
                }
                Err(failure)
              },
            }
          })
          .await;

          match outcome {
            Ok(v) => Ok(v),
            // Re-throw as a real `Error` carrying the original name and
            // message. `rquickjs::Error::new_from_js_message` would
            // render as "Error converting from js 'bdd' into type
            // 'Error': ...", and that prefix reaches the user through
            // every report the run writes.
            Err(e) => Err(crate::bindings::convert::throw_named_with_stack(
              &ctx,
              e.name(),
              e.message().to_string(),
              e.stack().map(ToString::to_string),
            )),
          }
        })
      },
    ),
  )
}

/// A JS exception, in the host-neutral shape core reports steps with.
pub(super) fn body_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> StepBodyError {
  match error {
    rquickjs::Error::Exception => {
      let caught = ctx.catch();
      let exception = caught.as_exception();
      StepBodyError {
        name: exception
          .and_then(|ex| ex.as_object().get::<_, String>("name").ok())
          .filter(|n| !n.is_empty())
          .unwrap_or_else(|| "Error".to_string()),
        message: exception
          .and_then(rquickjs::Exception::message)
          .unwrap_or_else(|| "step failed".to_string()),
        stack: exception.and_then(rquickjs::Exception::stack),
      }
    },
    other => StepBodyError::new("Error", other.to_string()),
  }
}

/// `{ box?, location?, timeout?, params?, subtitle? }` — Playwright's
/// option bag (`packages/playwright/types/test.d.ts`, `test.step`).
pub(super) fn parse_step_options<'js>(ctx: &Ctx<'js>, options: Option<&Value<'js>>) -> rquickjs::Result<StepOptions> {
  let Some(options) = options.and_then(Value::as_object) else {
    return Ok(StepOptions::default());
  };
  let mut parsed = StepOptions {
    box_step: options.get::<_, Option<bool>>("box")?.unwrap_or(false),
    location: None,
    timeout: options
      .get::<_, Option<f64>>("timeout")?
      .filter(|ms| *ms > 0.0)
      .map(|ms| std::time::Duration::from_millis(crate::bindings::convert::f64_to_u64(ms))),
    subtitle: options.get::<_, Option<String>>("subtitle")?,
    // Reports carry the params as JSON, which is what they are written as.
    params: match options.get::<_, Option<Object<'js>>>("params")? {
      Some(params) => ctx
        .json_stringify(params)?
        .map(|text| text.to_string())
        .transpose()?
        .and_then(|text| serde_json::from_str(&text).ok()),
      None => None,
    },
  };
  if let Some(location) = options.get::<_, Option<Object<'js>>>("location")? {
    let file: String = location.get("file").map_err(|_| {
      throw_script_error(
        ctx,
        &ScriptError::internal("test.step: location needs a `file`".to_string()),
      )
    })?;
    parsed.location = Some(StepLocation {
      file,
      line: crate::bindings::convert::f64_to_u32(location.get::<_, Option<f64>>("line")?.unwrap_or(0.0)),
      column: crate::bindings::convert::f64_to_u32(location.get::<_, Option<f64>>("column")?.unwrap_or(0.0)),
    });
  }
  let _ = ctx;
  Ok(parsed)
}

/// Playwright's `TestStepInfo` (`types/test.d.ts:10700-10816`):
/// `attach`, both `skip` forms, and `titlePath`. `annotations` is on
/// upstream's implementation but not in its public interface; it is
/// mirrored here as a real array so a push reaches the step's record.
pub(super) fn build_step_info<'js>(
  ctx: &Ctx<'js>,
  bridge: &Arc<dyn TestHostBridge>,
  run: &Arc<ferridriver_test::step::StepRun>,
) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("titlePath", run.title_path().to_vec())?;
  obj.set("annotations", rquickjs::Array::new(ctx.clone())?)?;

  let skip_run = Arc::clone(run);
  let skip = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
    // `skip()` unconditional; `skip(condition, description?)`.
    if let Some(condition) = args.0.first()
      && !condition
        .as_bool()
        .unwrap_or_else(|| !condition.is_null() && !condition.is_undefined())
    {
      return Ok(());
    }
    let description = args
      .0
      .get(1)
      .and_then(Value::as_string)
      .and_then(|s| s.to_string().ok());
    skip_run.record_skip(description.clone());
    Err::<(), _>(crate::bindings::convert::throw_named(
      &ctx,
      STEP_SKIP_ERROR,
      description.unwrap_or_else(|| "step skipped".to_string()),
    ))
  })?;
  obj.set("skip", skip)?;

  let attach_bridge = Arc::clone(bridge);
  let step_id = run.step_id().to_string();
  let attach = Function::new(
    ctx.clone(),
    Async(
      move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> Pin<Box<dyn Future<Output = rquickjs::Result<()>> + 'js>> {
        let bridge = Arc::clone(&attach_bridge);
        let step_id = step_id.clone();
        Box::pin(async move {
          let (name, content_type, source) = parse_attach_args(&ctx, &args.0)?;
          let bytes = match source {
            AttachSource::Bytes(b) => b,
            AttachSource::Path(p) => tokio::fs::read(&p).await.map_err(|e| {
              throw_script_error(
                &ctx,
                &ScriptError::internal(format!("stepInfo.attach: reading {p}: {e}")),
              )
            })?,
          };
          bridge.attach(name, content_type, bytes, Some(step_id)).await;
          Ok(())
        })
      },
    ),
  )?;
  obj.set("attach", attach)?;
  Ok(obj)
}

/// Fold whatever the body pushed onto `stepInfo.annotations` into the
/// step's record, so a reporter sees it.
pub(super) fn collect_step_annotations<'js>(step_info: &Object<'js>, run: &Arc<ferridriver_test::step::StepRun>) {
  let Ok(annotations) = step_info.get::<_, rquickjs::Array<'js>>("annotations") else {
    return;
  };
  for entry in annotations.iter::<Object<'js>>().flatten() {
    let Ok(type_name) = entry.get::<_, String>("type") else {
      continue;
    };
    run.annotate(type_name, entry.get::<_, Option<String>>("description").ok().flatten());
  }
}

pub(super) fn se(e: impl std::fmt::Display) -> ScriptError {
  ScriptError::internal(e.to_string())
}

/// Build or reuse the shared `testInfo` prototype carrying all getters and
/// methods backed by the VM's active test (`current_bridge`), avoiding 12
/// per-test closure/accessor allocations.
pub(super) fn test_info_proto<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>, ScriptError> {
  if let Some(saved) = with_test_registry(ctx, |r| r.test_info_proto.clone())? {
    return saved.restore(ctx).map_err(se);
  }
  let proto = Object::new(ctx.clone()).map_err(se)?;

  install_bridge_getter(
    ctx,
    &proto,
    "annotations",
    |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      let bridge = current_bridge(ctx, "testInfo.annotations")?;
      let existing = with_test_registry(ctx, |r| r.current.as_ref().and_then(|c| c.annotations.clone()))
        .map_err(|e| throw_script_error(ctx, &e))?;
      let mirror = if let Some(m) = existing {
        m
      } else {
        let created = AnnotationsMirror::new(ctx)?;
        with_test_registry(ctx, |r| {
          if let Some(c) = r.current.as_mut() {
            c.annotations = Some(created.clone());
          }
        })
        .map_err(|e| throw_script_error(ctx, &e))?;
        created
      };
      Ok(mirror.sync(ctx, bridge.as_ref())?.into_value())
    },
  )
  .map_err(se)?;
  install_bridge_getter(
    ctx,
    &proto,
    "attachmentCount",
    |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      let bridge = current_bridge(ctx, "testInfo.attachmentCount")?;
      rquickjs::IntoJs::into_js(u32::try_from(bridge.attachment_count()).unwrap_or(u32::MAX), ctx)
    },
  )
  .map_err(se)?;
  install_bridge_getter(
    ctx,
    &proto,
    "errors",
    |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      let bridge = current_bridge(ctx, "testInfo.errors")?;
      rquickjs::IntoJs::into_js(bridge.errors(), ctx)
    },
  )
  .map_err(se)?;

  proto.set("attach", make_attach_fn(ctx)?).map_err(se)?;

  let annotate = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, kind: String, description: rquickjs::function::Opt<String>| -> rquickjs::Result<()> {
      current_bridge(&ctx, "testInfo.annotate()")?.annotate(kind, description.0);
      Ok(())
    },
  )
  .map_err(se)?;
  proto.set("annotate", annotate).map_err(se)?;

  for kind in ["skip", "fixme", "fail", "slow"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      runtime_modifier(&ctx, kind, &args.0)
    })
    .map_err(se)?;
    proto.set(kind, f).map_err(se)?;
  }
  let set_timeout = Function::new(ctx.clone(), |ctx: Ctx<'js>, ms: f64| -> rquickjs::Result<()> {
    current_bridge(&ctx, "testInfo.setTimeout()")?.set_timeout_override(crate::bindings::convert::f64_to_u64(ms));
    Ok(())
  })
  .map_err(se)?;
  proto.set("setTimeout", set_timeout).map_err(se)?;

  let output_path = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, parts: Rest<String>| -> rquickjs::Result<String> {
      Ok(current_bridge(&ctx, "testInfo.outputPath()")?.output_path(&parts.0))
    },
  )
  .map_err(se)?;
  proto.set("outputPath", output_path).map_err(se)?;
  proto.set("snapshotPath", make_snapshot_path_fn(ctx)?).map_err(se)?;

  let saved = Persistent::save(ctx, proto.clone());
  with_test_registry(ctx, |r| {
    r.test_info_proto = Some(saved);
  })?;
  Ok(proto)
}

pub(super) fn build_test_info<'js>(ctx: &Ctx<'js>, info: &TestInfoData) -> Result<Object<'js>, ScriptError> {
  let proto = test_info_proto(ctx)?;
  let obj = Object::new(ctx.clone()).map_err(se)?;
  obj.set_prototype(Some(&proto)).map_err(se)?;
  set_test_info_fields(ctx, &obj, info)?;
  Ok(obj)
}

/// The plain data fields of `testInfo`.
pub(super) fn set_test_info_fields<'js>(
  ctx: &Ctx<'js>,
  obj: &Object<'js>,
  info: &TestInfoData,
) -> Result<(), ScriptError> {
  obj.set("title", info.title.as_str()).map_err(se)?;
  obj.set("titlePath", info.title_path.as_slice()).map_err(se)?;
  obj.set("file", info.file.as_str()).map_err(se)?;
  obj.set("line", info.line).map_err(se)?;
  obj.set("column", info.column).map_err(se)?;
  obj.set("retry", info.retry).map_err(se)?;
  obj.set("workerIndex", info.worker_index).map_err(se)?;
  obj.set("parallelIndex", info.parallel_index).map_err(se)?;
  obj.set("repeatEachIndex", info.repeat_each_index).map_err(se)?;
  obj.set("timeout", info.timeout_ms).map_err(se)?;
  obj.set("expectedStatus", info.expected_status.as_str()).map_err(se)?;
  obj.set("tags", info.tags.as_slice()).map_err(se)?;
  obj.set("outputDir", info.output_dir.as_str()).map_err(se)?;
  obj.set("snapshotDir", info.snapshot_dir.as_str()).map_err(se)?;
  obj.set("snapshotSuffix", info.snapshot_suffix.as_str()).map_err(se)?;
  let config_val = if let Some(cached) = with_test_registry(ctx, |r| {
    r.cached_config
      .as_ref()
      .filter(|(raw, _)| *raw == info.config)
      .map(|(_, val)| val.clone())
  })? {
    cached.restore(ctx).map_err(se)?
  } else {
    let val = crate::bindings::convert::json_to_js(ctx, &info.config).map_err(se)?;
    let saved = Persistent::save(ctx, val.clone());
    with_test_registry(ctx, |r| {
      r.cached_config = Some((info.config.clone(), saved));
    })?;
    val
  };
  obj.set("config", config_val).map_err(se)?;
  // A config without projects runs Playwright's one unnamed project, so
  // `testInfo.project` is always there, its name empty.
  let project = Object::new(ctx.clone()).map_err(se)?;
  project
    .set("name", info.project_name.as_deref().unwrap_or(""))
    .map_err(se)?;
  obj.set("project", project).map_err(se)?;
  Ok(())
}

/// `testInfo.attach`.
pub(super) fn make_attach_fn<'js>(ctx: &Ctx<'js>) -> Result<Function<'js>, ScriptError> {
  Function::new(
    ctx.clone(),
    Async(
      |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> Pin<Box<dyn Future<Output = rquickjs::Result<()>> + 'js>> {
        Box::pin(async move {
          let bridge = current_bridge(&ctx, "testInfo.attach()")?;
          let (name, content_type, source) = parse_attach_args(&ctx, &args.0)?;
          let bytes = match source {
            AttachSource::Bytes(b) => b,
            AttachSource::Path(p) => tokio::fs::read(&p).await.map_err(|e| {
              throw_script_error(
                &ctx,
                &ScriptError::internal(format!("testInfo.attach: reading {p}: {e}")),
              )
            })?,
          };
          bridge.attach(name, content_type, bytes, None).await;
          Ok(())
        })
      },
    ),
  )
  .map_err(se)
}

/// `testInfo.snapshotPath(...)`.
pub(super) fn make_snapshot_path_fn<'js>(ctx: &Ctx<'js>) -> Result<Function<'js>, ScriptError> {
  // Playwright: `snapshotPath(...name: string[])` and
  // `snapshotPath(name, { kind })` (`worker/testInfo.ts:644-662`) — a
  // trailing object is the options bag, everything before it is a path.
  Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<String> {
      let snap_bridge = current_bridge(&ctx, "testInfo.snapshotPath()")?;
      let mut parts: Vec<String> = Vec::with_capacity(args.0.len());
      let mut kind = "snapshot".to_string();
      let last = args.0.len().saturating_sub(1);
      for (index, value) in args.0.iter().enumerate() {
        if index == last
          && let Some(obj) = value.as_object()
          && !value.is_string()
        {
          if let Some(k) = obj.get::<_, Option<String>>("kind")? {
            kind = k;
          }
          break;
        }
        if let Some(s) = value.as_string() {
          parts.push(s.to_string()?);
        }
      }
      snap_bridge
        .snapshot_path(&parts, &kind)
        .map_err(|message| throw_script_error(&ctx, &ScriptError::internal(message)))
    },
  )
  .map_err(se)
}

pub(super) fn install_bridge_getter<'js>(
  ctx: &Ctx<'js>,
  target: &Object<'js>,
  name: &str,
  getter: impl Fn(&Ctx<'js>) -> rquickjs::Result<Value<'js>> + 'js,
) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let getter_fn = Function::new(ctx.clone(), move |ctx: Ctx<'js>| getter(&ctx))?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter_fn)?;
  descriptor.set("configurable", true)?;
  let _: Value<'js> = define_property.call((target.clone(), name, descriptor))?;
  Ok(())
}

pub(super) enum AttachSource {
  Bytes(Vec<u8>),
  Path(String),
}

pub(super) fn parse_attach_args<'js>(
  ctx: &Ctx<'js>,
  args: &[Value<'js>],
) -> rquickjs::Result<(String, String, AttachSource)> {
  let name = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| {
      throw_script_error(
        ctx,
        &ScriptError::internal("testInfo.attach: name must be a string".to_string()),
      )
    })?
    .to_string()?;
  // Positional: (name, contentType, body[, opts]).
  if let Some(ct) = args.get(1).and_then(Value::as_string) {
    let content_type = ct.to_string()?;
    let body = args
      .get(2)
      .ok_or_else(|| throw_script_error(ctx, &ScriptError::internal("testInfo.attach: missing body".to_string())))?;
    let bytes = ferrijs::std::node::bytes::value_to_bytes(ctx, body, None)?;
    return Ok((name, content_type, AttachSource::Bytes(bytes)));
  }
  // Option bag: (name, { body?, contentType?, path? }).
  let Some(opts) = args.get(1).and_then(Value::as_object) else {
    return Err(throw_script_error(
      ctx,
      &ScriptError::internal(
        "testInfo.attach: second argument must be a content type or an options object".to_string(),
      ),
    ));
  };
  let content_type = opts
    .get::<_, Value<'js>>("contentType")
    .ok()
    .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()));
  if let Ok(path) = opts.get::<_, String>("path") {
    crate::bindings::convert::readable(ctx, std::path::Path::new(&path))?;
    let content_type = content_type.unwrap_or_else(|| "application/octet-stream".to_string());
    return Ok((name, content_type, AttachSource::Path(path)));
  }
  let body: Value<'js> = opts.get("body").map_err(|_| {
    throw_script_error(
      ctx,
      &ScriptError::internal("testInfo.attach: options need `body` or `path`".to_string()),
    )
  })?;
  let default_ct = if body.as_string().is_some() {
    "text/plain"
  } else {
    "application/octet-stream"
  };
  let bytes = ferrijs::std::node::bytes::value_to_bytes(ctx, &body, None)?;
  Ok((
    name,
    content_type.unwrap_or_else(|| default_ct.to_string()),
    AttachSource::Bytes(bytes),
  ))
}

/// Build the per-test fixtures object (`{ page, context, request,
/// browser, browserName, headless, isMobile, hasTouch, testInfo, ...
/// custom }`) and register it as the VM's current test.
pub(crate) fn set_current_test(
  ctx: &Ctx<'_>,
  vm: &ferrijs::VmHandle,
  world: &TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
) -> Result<(), ScriptError> {
  if let Some(owner) = bridge.call_owner() {
    crate::bindings::call_site::set_script_id(ctx, &owner);
  }
  let obj = Object::new(ctx.clone()).map_err(se)?;
  obj.set("browserName", world.browser_name.as_str()).map_err(se)?;
  obj.set("headless", world.headless).map_err(se)?;
  obj.set("isMobile", world.is_mobile).map_err(se)?;
  obj.set("hasTouch", world.has_touch).map_err(se)?;
  obj.set("baseURL", world.base_url.as_deref()).map_err(se)?;
  if let Some(page) = &world.page {
    install_page_on(ctx, &obj, Arc::clone(page), vm.clone()).map_err(se)?;
  }
  if let Some(c) = &world.context {
    install_browser_context_on(ctx, &obj, Arc::clone(c)).map_err(se)?;
  }
  if let Some(r) = &world.request {
    install_request_on(ctx, &obj, Arc::clone(r)).map_err(se)?;
  }
  if let Some(b) = &world.browser {
    install_browser_on(ctx, &obj, Arc::clone(b)).map_err(se)?;
  }
  let info_obj = build_test_info(ctx, &world.info)?;
  obj.set("testInfo", info_obj.clone()).map_err(se)?;
  // The `expect` block this test's project resolved to — every bare
  // `expect(...)` the body makes starts from its timeout.
  crate::bindings::expect::set_expect_defaults(ctx, Arc::clone(&world.expect));

  let world_saved = Persistent::save(ctx, obj);
  let info_saved = Persistent::save(ctx, info_obj);
  with_test_registry(ctx, |r| {
    r.current = Some(CurrentTest {
      world: world_saved,
      test_info: info_saved,
      bridge,
      pending: Vec::new(),
      annotations: None,
    });
  })
}

/// The `test.extend` chain for a fixture set, in extend order — the
/// input to [`fixture_graph`]'s Playwright-shaped resolution.
pub(crate) fn fixture_slots(reg: &TestRegistry, fixture_set: usize) -> Vec<FixtureSlot> {
  reg
    .fixture_sets
    .get(fixture_set)
    .map(Vec::as_slice)
    .unwrap_or_default()
    .iter()
    .map(|&i| {
      let f = &reg.fixtures[i];
      FixtureSlot {
        reg: i,
        name: f.name.clone(),
        deps: f.deps.clone(),
        auto: f.auto,
        scope: f.scope,
        option: f.option,
      }
    })
    .collect()
}

/// Snapshot of the `test.extend` chains registered in this VM, indexed
/// by fixture set — the input [`dominant_fixture_set`] reasons over.
pub(crate) fn fixture_set_table(reg: &TestRegistry) -> Vec<Vec<usize>> {
  reg.fixture_sets.clone()
}

/// Where the test surface's registries stand — `(tests, fixtures)`.
pub(crate) fn registry_marks(ctx: &Ctx<'_>) -> Result<(usize, usize), ScriptError> {
  with_test_registry(ctx, |r| (r.tests.len(), r.fixtures.len()))
}

/// Test titles and fixture names registered since those marks.
pub(crate) fn registrations_since(
  ctx: &Ctx<'_>,
  tests: usize,
  fixtures: usize,
) -> Result<(Vec<String>, Vec<String>), ScriptError> {
  with_test_registry(ctx, |r| {
    (
      r.tests
        .get(tests..)
        .unwrap_or_default()
        .iter()
        .map(|t| t.title.clone())
        .collect(),
      r.fixtures
        .get(fixtures..)
        .unwrap_or_default()
        .iter()
        .map(|f| f.name.clone())
        .collect(),
    )
  })
}

/// Positional registrations on the test surface: tests, suites, hooks
/// and fixtures. Consumers address all of them by index, so a partial
/// registration is not something a host can skip past.
pub(crate) fn registration_count(ctx: &Ctx<'_>) -> Result<usize, ScriptError> {
  with_test_registry(ctx, |r| {
    r.tests.len() + r.suites.len() + r.hooks.len() + r.fixtures.len() + r.modifiers.len()
  })
}

/// Every fixture registration in this VM, in registration order — the
/// table [`fixture_set_table`]'s indices point into.
pub(crate) fn fixture_table(reg: &TestRegistry) -> Vec<CollectedFixture> {
  reg
    .fixtures
    .iter()
    .map(|f| CollectedFixture {
      name: f.name.clone(),
      scope: f.scope,
      auto: f.auto,
      option: f.option,
      deps: f.deps.clone(),
    })
    .collect()
}

/// Clear the current-test slot (the BDD host ends a scenario without
/// going through [`run_test`]).
pub(crate) fn clear_current_test(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  with_test_registry(ctx, |r| r.current = None)
}

/// The fixtures object of the running test / scenario, once
/// [`set_current_test`] built it.
pub(crate) fn current_world_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>, ScriptError> {
  let saved = with_test_registry(ctx, |r| r.current.as_ref().map(|c| c.world.clone()))?
    .ok_or_else(|| ScriptError::internal("no current test".to_string()))?;
  saved.restore(ctx).map_err(se)
}
