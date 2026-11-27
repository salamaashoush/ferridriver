//! `test` and `describe` callable object builders (`skip`, `only`, `fixme`,
//! `fail`, `slow`, `each`, `use`, `extend`, `info`, hooks, `describe.configure`).

use rquickjs::function::Rest;
use rquickjs::{Ctx, Function, Object, Persistent, Value};

use crate::bindings::convert::serde_from_js;
use crate::bindings::registry::{as_function, throw_script_error};
use crate::error::ScriptError;

use super::registry::{
  CollectedAnnotation, CollectedSuiteMode, FileAnnotationReg, FileConfigureReg, FileUseReg, SuiteReg, TestHookReg,
  append_fixture, capture_location, current_bridge, destructured_keys, fixture_set_of, interpolate_title,
  lock_annotations, modifier_annotation, modifier_call, parse_details, parse_fixture_entry, parse_registration,
  push_test, test_type_key, with_test_registry,
};
use super::step::make_step_fn;

// ── The test / describe object builders ──────────────────────────────

/// Build a `test` function object bound to one fixture set. `test.extend`
/// builds a fresh object over the extended set, so the full surface
/// (including further `.extend`) chains naturally.
pub(super) fn make_test_object<'js>(ctx: &Ctx<'js>, fixture_set: usize) -> rquickjs::Result<Function<'js>> {
  let set = fixture_set;
  let test_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
      let reg = parse_registration(&ctx, &args.0)?;
      push_test(&ctx, set, &[], reg, None, None)
    },
  )?;
  test_fn.set_name("test")?;
  let obj = test_fn.as_object().ok_or_else(|| {
    throw_script_error(
      ctx,
      &ScriptError::internal("test function has no object form".to_string()),
    )
  })?;

  for kind in ["skip", "fixme", "fail", "slow"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      modifier_call(kind, set, &ctx, &args.0)
    })?;
    obj.set(kind, f)?;
  }

  let only = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
      let reg = parse_registration(&ctx, &args.0)?;
      push_test(&ctx, set, &[modifier_annotation("only", None)], reg, None, None)
    },
  )?;
  obj.set("only", only)?;

  obj.set("each", make_each_fn(ctx, set)?)?;

  for kind in ["beforeAll", "afterAll", "beforeEach", "afterEach"] {
    obj.set(kind, make_hook_fn(ctx, kind, set)?)?;
  }

  obj.set("use", make_use_fn(ctx)?)?;

  let set_timeout = Function::new(ctx.clone(), |ctx: Ctx<'js>, ms: f64| -> rquickjs::Result<()> {
    current_bridge(&ctx, "test.setTimeout()")?.set_timeout_override(crate::bindings::convert::f64_to_u64(ms));
    Ok(())
  })?;
  obj.set("setTimeout", set_timeout)?;

  obj.set("info", make_info_fn(ctx)?)?;

  obj.set("step", make_step_fn(ctx)?)?;

  obj.set("extend", make_extend_fn(ctx, set)?)?;

  let describe = make_describe_object(ctx)?;
  obj.set("describe", describe)?;

  // Non-enumerable, non-writable: a spread of `test` must not copy it,
  // and a suite must not be able to forge a chain by assigning it.
  obj.prop(
    test_type_key(ctx)?,
    rquickjs::object::Property::from(crate::bindings::convert::usize_to_f64(set)),
  )?;

  Ok(test_fn)
}

/// `test.each(rows)`: a registrar that adds one test per row.
pub(super) fn make_each_fn<'js>(ctx: &Ctx<'js>, set: usize) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, rows: Value<'js>| -> rquickjs::Result<Function<'js>> {
      let rows: Vec<serde_json::Value> = serde_from_js(&ctx, rows).map_err(|e| {
        throw_script_error(
          &ctx,
          &ScriptError::internal(format!("test.each rows must be an array: {e}")),
        )
      })?;
      Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
          for row in &rows {
            let reg = parse_registration(&ctx, &args.0)?;
            let title = interpolate_title(&reg.title, row);
            push_test(&ctx, set, &[], reg, Some(row.clone()), Some(title))?;
          }
          Ok(())
        },
      )
    },
  )
}

/// `test.beforeAll` and its siblings, by `kind`.
pub(super) fn make_hook_fn<'js>(
  ctx: &Ctx<'js>,
  kind: &'static str,
  fixture_set: usize,
) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
      // `(fn)` or `(title, fn)` — the optional title is display-only.
      let func = args.0.iter().find_map(as_function).ok_or_else(|| {
        throw_script_error(
          &ctx,
          &ScriptError::internal(format!("test.{kind} has no function body")),
        )
      })?;
      let (line, col) = capture_location(&ctx);
      let requested = destructured_keys(&ctx, &func);
      let saved = Persistent::save(&ctx, func);
      with_test_registry(&ctx, |r| {
        r.hooks.push(TestHookReg {
          kind: kind.to_string(),
          suite: r.describe_stack.last().copied(),
          fixture_set,
          func: saved,
          requested,
          line,
          col,
        });
      })
      .map_err(|e| throw_script_error(&ctx, &e))
    },
  )
}

/// `test.use(options)`.
pub(super) fn make_use_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  Function::new(ctx.clone(), |ctx: Ctx<'js>, bag: Value<'js>| -> rquickjs::Result<()> {
    let options: serde_json::Value = serde_from_js(&ctx, bag).map_err(|e| {
      throw_script_error(
        &ctx,
        &ScriptError::internal(format!("test.use options must be a plain object: {e}")),
      )
    })?;
    if !options.is_object() {
      return Err(throw_script_error(
        &ctx,
        &ScriptError::internal("test.use options must be a plain object".to_string()),
      ));
    }
    let (line, col) = capture_location(&ctx);
    with_test_registry(&ctx, |r| match r.describe_stack.last().copied() {
      Some(idx) => merge_use(&mut r.suites[idx].use_options, &options),
      None => r.file_use.push(FileUseReg { options, line, col }),
    })
    .map_err(|e| throw_script_error(&ctx, &e))
  })
}

/// `test.info()`.
pub(super) fn make_info_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
    let saved = with_test_registry(&ctx, |r| r.current.as_ref().map(|c| c.test_info.clone()))
      .map_err(|e| throw_script_error(&ctx, &e))?;
    let saved = saved.ok_or_else(|| {
      throw_script_error(
        &ctx,
        &ScriptError::internal("test.info() can only be called while a test is running".to_string()),
      )
    })?;
    saved
      .restore(&ctx)
      .map_err(|e| throw_script_error(&ctx, &ScriptError::internal(e.to_string())))
  })
}

/// `test.extend(fixtures)`: a new test object over a widened fixture set.
pub(super) fn make_extend_fn<'js>(ctx: &Ctx<'js>, set: usize) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, fixtures: Object<'js>| -> rquickjs::Result<Function<'js>> {
      if fixture_set_of(&ctx, fixtures.as_value()).is_some() {
        return Err(throw_script_error(
          &ctx,
          &ScriptError::internal(
            "test.extend() accepts fixtures object, not a test object.\nDid you mean to call mergeTests()?".to_string(),
          ),
        ));
      }
      let mut new_regs = Vec::new();
      for key in fixtures.keys::<String>() {
        let key = key?;
        let v: Value<'js> = fixtures.get(key.as_str())?;
        new_regs.push(parse_fixture_entry(&ctx, &key, v)?);
      }
      let new_set = with_test_registry(&ctx, |r| {
        let mut visible = r.fixture_sets.get(set).cloned().unwrap_or_default();
        for parsed in new_regs {
          append_fixture(r, &mut visible, parsed)?;
        }
        r.fixture_sets.push(visible);
        Ok(r.fixture_sets.len() - 1)
      })
      .map_err(|e| throw_script_error(&ctx, &e))?
      .map_err(|e: ScriptError| throw_script_error(&ctx, &e))?;
      make_test_object(&ctx, new_set)
    },
  )
}

/// Shallow `Object.assign` merge of a `use` bag into a target slot.
pub(super) fn merge_use(target: &mut Option<serde_json::Value>, incoming: &serde_json::Value) {
  match target {
    Some(serde_json::Value::Object(t)) => {
      if let serde_json::Value::Object(inc) = incoming {
        for (k, v) in inc {
          t.insert(k.clone(), v.clone());
        }
      }
    },
    _ => *target = Some(incoming.clone()),
  }
}

pub(super) fn register_suite(
  ctx: &Ctx<'_>,
  name: &str,
  mode: Option<CollectedSuiteMode>,
  annotations: Vec<CollectedAnnotation>,
) -> rquickjs::Result<usize> {
  let (line, col) = capture_location(ctx);
  if annotations.iter().any(|a| a.kind == "only") {
    with_test_registry(ctx, |r| r.has_only = true).map_err(|e| throw_script_error(ctx, &e))?;
  }
  with_test_registry(ctx, |r| {
    let parent = r.describe_stack.last().copied();
    r.suites.push(SuiteReg {
      name: name.to_string(),
      parent,
      mode,
      annotations,
      use_options: None,
      retries: None,
      timeout_ms: None,
      line,
      col,
    });
    let idx = r.suites.len() - 1;
    r.describe_stack.push(idx);
    idx
  })
  .map_err(|e| throw_script_error(ctx, &e))
}

pub(super) fn pop_suite(ctx: &Ctx<'_>) {
  let _ = with_test_registry(ctx, |r| {
    r.describe_stack.pop();
  });
}

/// `describe(name, fn)` with a mode / annotation preset — the body runs
/// synchronously with the new suite on the registration stack.
pub(super) fn describe_call<'js>(
  ctx: &Ctx<'js>,
  args: &[Value<'js>],
  mode: Option<CollectedSuiteMode>,
  annotations: Vec<CollectedAnnotation>,
) -> rquickjs::Result<()> {
  let name = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| {
      throw_script_error(
        ctx,
        &ScriptError::internal("describe title must be a string".to_string()),
      )
    })?
    .to_string()?;
  let body = args.iter().skip(1).find_map(as_function).ok_or_else(|| {
    throw_script_error(
      ctx,
      &ScriptError::internal(format!("describe `{name}` has no function body")),
    )
  })?;
  // Playwright: `describe(title, details, callback)`; the group's tags,
  // annotations and locks reach every test in it.
  let mut annotations = annotations;
  if let Some(details) = args
    .get(1)
    .filter(|v| v.as_function().is_none() && v.as_object().is_some())
  {
    annotations.extend(parse_details(ctx, details)?.annotations);
  }
  register_suite(ctx, &name, mode, annotations)?;
  let result = body.call::<_, ()>(());
  pop_suite(ctx);
  result
}

pub(super) fn make_describe_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  let describe_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> { describe_call(&ctx, &args.0, None, Vec::new()) },
  )?;
  describe_fn.set_name("describe")?;
  let obj = describe_fn.as_object().ok_or_else(|| {
    throw_script_error(
      ctx,
      &ScriptError::internal("describe function has no object form".to_string()),
    )
  })?;

  for (name, mode) in [
    ("serial", CollectedSuiteMode::Serial),
    ("parallel", CollectedSuiteMode::Parallel),
  ] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      describe_call(&ctx, &args.0, Some(mode), Vec::new())
    })?;
    obj.set(name, f)?;
  }

  for kind in ["skip", "fixme", "only"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      describe_call(&ctx, &args.0, None, vec![modifier_annotation(kind, None)])
    })?;
    obj.set(kind, f)?;
  }

  obj.set("each", make_describe_each_fn(ctx)?)?;

  obj.set("configure", make_describe_configure_fn(ctx)?)?;

  Ok(describe_fn)
}

/// `describe.each(rows)`: a registrar that adds one suite per row.
pub(super) fn make_describe_each_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, rows: Value<'js>| -> rquickjs::Result<Function<'js>> {
      let rows: Vec<serde_json::Value> = serde_from_js(&ctx, rows).map_err(|e| {
        throw_script_error(
          &ctx,
          &ScriptError::internal(format!("describe.each rows must be an array: {e}")),
        )
      })?;
      Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
          let template = args
            .0
            .first()
            .and_then(Value::as_string)
            .ok_or_else(|| {
              throw_script_error(
                &ctx,
                &ScriptError::internal("describe.each title must be a string".to_string()),
              )
            })?
            .to_string()?;
          let body = args.0.iter().skip(1).find_map(as_function).ok_or_else(|| {
            throw_script_error(
              &ctx,
              &ScriptError::internal(format!("describe.each `{template}` has no function body")),
            )
          })?;
          for row in &rows {
            let title = interpolate_title(&template, row);
            register_suite(&ctx, &title, None, Vec::new())?;
            let row_js = crate::bindings::convert::json_to_js(&ctx, row)?;
            let result = body.call::<_, ()>((row_js,));
            pop_suite(&ctx);
            result?;
          }
          Ok(())
        },
      )
    },
  )
}

/// `describe.configure({ mode, retries, timeout })`.
pub(super) fn make_describe_configure_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, opts: Object<'js>| -> rquickjs::Result<()> {
      let mode = match opts.get::<_, String>("mode") {
        Ok(m) => Some(match m.as_str() {
          "serial" => CollectedSuiteMode::Serial,
          "parallel" => CollectedSuiteMode::Parallel,
          "default" => return Ok(()),
          other => {
            return Err(throw_script_error(
              &ctx,
              &ScriptError::internal(format!("describe.configure: unknown mode `{other}`")),
            ));
          },
        }),
        Err(_) => None,
      };
      let retries = opts
        .get::<_, f64>("retries")
        .ok()
        .map(crate::bindings::convert::f64_to_u32);
      let timeout_ms = opts
        .get::<_, f64>("timeout")
        .ok()
        .map(crate::bindings::convert::f64_to_u64);
      let locks = lock_annotations(&ctx, &opts)?;
      let (line, col) = capture_location(&ctx);
      with_test_registry(&ctx, |r| {
        if let Some(idx) = r.describe_stack.last().copied() {
          let s = &mut r.suites[idx];
          s.annotations.extend(locks);
          if mode.is_some() {
            s.mode = mode;
          }
          if retries.is_some() {
            s.retries = retries;
          }
          if timeout_ms.is_some() {
            s.timeout_ms = timeout_ms;
          }
        } else {
          r.file_configure.push(FileConfigureReg {
            mode,
            retries,
            timeout_ms,
            line,
            col,
          });
          r.file_annotations.extend(
            locks
              .into_iter()
              .map(|annotation| FileAnnotationReg { annotation, line, col }),
          );
        }
      })
      .map_err(|e| throw_script_error(&ctx, &e))
    },
  )
}
