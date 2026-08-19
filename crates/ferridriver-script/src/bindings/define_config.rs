//! `defineConfig` — the config-authoring merge a `--config <file.ts>`
//! module folds its layers with.
//!
//! Ported from `packages/playwright/src/common/configLoader.ts:32-87`.
//! It is a JS-object merge, so it is a JS-object merge here too: the
//! arguments are author-written config literals whose values include
//! functions, explicit `undefined` and shared references, none of which
//! survive a round trip through JSON. What ferridriver does with the
//! merged document afterwards — which layer slot it occupies, which
//! sections it may not set — is the config crate's, not this file's.
//!
//! Not ported: upstream's `kDefineConfigWasUsed` marker symbol. It is
//! written onto the result and read by nothing in the Playwright tree,
//! so mirroring it would add an observable property that decides
//! nothing.

use rquickjs::function::Rest;
use rquickjs::{Array, Ctx, Function, Object, Value};

use crate::bindings::convert::throw_named;

/// Keys whose object value is merged one level deep instead of being
/// replaced wholesale. Upstream spells each one out; the list is the
/// contract, so it lives as data rather than three copies of the same
/// four lines.
// `build` is Playwright's; ferridriver has no such section, and
// inventing an empty one made every config module emit a document key
// its own schema then reported as unknown.
const SHALLOW_MERGED_KEYS: &[&str] = &["expect", "use"];

/// Install `defineConfig` on the `ferridriver` global, where the
/// `@ferridriver/test` native module reads it from.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let fd = crate::bindings::runtime::ensure_ferridriver(ctx)?;
  let f = Function::new(ctx.clone(), define_config)?;
  f.set_name("defineConfig")?;
  fd.set("defineConfig", f)?;
  Ok(())
}

/// `defineConfig(config, ...configs)` — fold left, so the RIGHTMOST
/// argument wins for every scalar key.
///
/// A single argument is returned as it was passed, not copied: upstream
/// assigns `result = configs[0]` and never clones it, and a config that
/// arrived by reference has to keep going out by reference.
fn define_config<'js>(ctx: Ctx<'js>, configs: Rest<Value<'js>>) -> rquickjs::Result<Value<'js>> {
  let mut iter = configs.0.into_iter();
  let Some(first) = iter.next() else {
    // Upstream throws here too, from `result[kDefineConfigWasUsed] = true`
    // on `undefined`; it just cannot say what went wrong.
    return Err(throw_named(
      &ctx,
      "TypeError",
      "defineConfig() requires at least one config object".to_string(),
    ));
  };
  let mut result = first;
  for config in iter {
    result = merge_one(&ctx, &result, &config)?;
  }
  Ok(result)
}

/// One `{...result, ...config}` step with the four keys upstream treats
/// specially.
fn merge_one<'js>(ctx: &Ctx<'js>, prev: &Value<'js>, next: &Value<'js>) -> rquickjs::Result<Value<'js>> {
  // Read before the spread: the spread overwrites `projects` with the
  // incoming list, and the by-name merge below needs the outgoing one.
  let prev_projects = get(prev, "projects")?;

  let out = Object::new(ctx.clone())?;
  spread_into(&out, prev)?;
  spread_into(&out, next)?;

  for key in SHALLOW_MERGED_KEYS {
    let merged = Object::new(ctx.clone())?;
    spread_into(&merged, &get(prev, key)?)?;
    spread_into(&merged, &get(next, key)?)?;
    out.set(*key, merged)?;
  }

  let web_server = Array::new(ctx.clone())?;
  let mut at = 0;
  for side in [prev, next] {
    for entry in as_list(&get(side, "webServer")?)? {
      web_server.set(at, entry)?;
      at += 1;
    }
  }
  out.set("webServer", web_server)?;

  let next_projects = get(next, "projects")?;
  // Upstream's `if (!result.projects && !config.projects) continue`,
  // read after the spread has already run — so the left side is
  // `config.projects` when the incoming config has the key at all.
  if is_falsy(&out.get::<_, Value<'js>>("projects")?) && is_falsy(&next_projects) {
    return Ok(out.into_value());
  }

  out.set("projects", merge_projects(ctx, &prev_projects, &next_projects)?)?;
  Ok(out.into_value())
}

/// Projects merge by NAME: an incoming project with a name the outgoing
/// list already has overrides it in place (its own `use` shallow-merged
/// on top), and a name nothing matched is appended in the order the
/// incoming config wrote it.
fn merge_projects<'js>(ctx: &Ctx<'js>, prev: &Value<'js>, next: &Value<'js>) -> rquickjs::Result<Array<'js>> {
  // Upstream's Map: insertion-ordered, and an entry is deleted once it
  // has overridden something, so what is left over is what gets
  // appended. A Vec of (key, value, taken) is that Map.
  let mut overrides: Vec<(Option<String>, Value<'js>, bool)> = Vec::new();
  for project in project_list(next)? {
    let name = project_name(&project)?;
    if let Some(existing) = overrides.iter_mut().find(|(n, _, _)| *n == name) {
      existing.1 = project;
    } else {
      overrides.push((name, project, false));
    }
  }

  let out = Array::new(ctx.clone())?;
  let mut at = 0;
  for project in project_list(prev)? {
    let name = project_name(&project)?;
    if let Some(entry) = overrides.iter_mut().find(|(n, _, taken)| !*taken && *n == name) {
      entry.2 = true;
      let merged = Object::new(ctx.clone())?;
      spread_into(&merged, &project)?;
      spread_into(&merged, &entry.1)?;
      let use_block = Object::new(ctx.clone())?;
      spread_into(&use_block, &get(&project, "use")?)?;
      spread_into(&use_block, &get(&entry.1, "use")?)?;
      merged.set("use", use_block)?;
      out.set(at, merged)?;
    } else {
      // Never rebuilt: a project nothing overrode keeps its identity,
      // and with it a `use` that was absent rather than `{}`.
      out.set(at, project)?;
    }
    at += 1;
  }
  for (_, project, taken) in overrides {
    if !taken {
      out.set(at, project)?;
      at += 1;
    }
  }
  Ok(out)
}

/// A project's `name`, as the key the override map is built on.
///
/// `Project.name` is `string | undefined` in Playwright's own
/// declaration, so a non-string lands in the same unnamed bucket
/// `undefined` does rather than raising — a merge is not the place to
/// discover that a config is mistyped.
fn project_name(project: &Value<'_>) -> rquickjs::Result<Option<String>> {
  let name = get(project, "name")?;
  Ok(name.as_string().and_then(|s| s.to_string().ok()))
}

/// `obj[key]`, walking the prototype chain the way a JS property read
/// does. `undefined` for anything that is not an object.
fn get<'js>(value: &Value<'js>, key: &str) -> rquickjs::Result<Value<'js>> {
  match value.as_object() {
    Some(obj) => obj.get(key),
    None => Ok(Value::new_undefined(value.ctx().clone())),
  }
}

/// `{...target, ...source}`: copy `source`'s own enumerable string-keyed
/// properties onto `target`, including the ones whose value is an
/// explicit `undefined` — that is what lets a later layer erase an
/// earlier one's key.
///
/// A primitive source contributes nothing, as `{...undefined}` and
/// `{...42}` do. (A spread string would contribute its indices; a config
/// key holding a string where an object belongs is already broken, and
/// silently indexing it would hide that.)
fn spread_into<'js>(target: &Object<'js>, source: &Value<'js>) -> rquickjs::Result<()> {
  let Some(obj) = source.as_object() else {
    return Ok(());
  };
  for entry in obj.props::<String, Value<'js>>() {
    let (key, value) = entry?;
    target.set(key, value)?;
  }
  Ok(())
}

/// `projects`, which upstream spells `config.projects || []` and then
/// iterates — so anything truthy that is not an array is a config bug
/// that upstream discovers as "is not iterable". Named here instead.
fn project_list<'js>(value: &Value<'js>) -> rquickjs::Result<Vec<Value<'js>>> {
  if let Some(array) = value.as_array() {
    return array.iter::<Value<'js>>().collect();
  }
  if is_falsy(value) {
    return Ok(Vec::new());
  }
  Err(throw_named(
    value.ctx(),
    "TypeError",
    "defineConfig(): `projects` must be an array of project objects".to_string(),
  ))
}

/// `Array.isArray(v) ? v : (v ? [v] : [])` — the normalize half of
/// `webServer`'s normalize-and-concatenate. A lone object is a webServer,
/// which is a shape Playwright's own type allows.
fn as_list<'js>(value: &Value<'js>) -> rquickjs::Result<Vec<Value<'js>>> {
  if let Some(array) = value.as_array() {
    return array.iter::<Value<'js>>().collect();
  }
  if is_falsy(value) {
    return Ok(Vec::new());
  }
  Ok(vec![value.clone()])
}

/// JS `ToBoolean`, for the two truthiness tests upstream makes on values
/// it has not typed.
fn is_falsy(value: &Value<'_>) -> bool {
  if value.is_undefined() || value.is_null() {
    return true;
  }
  if let Some(b) = value.as_bool() {
    return !b;
  }
  if let Some(n) = value.as_number() {
    return n == 0.0 || n.is_nan();
  }
  if let Some(s) = value.as_string() {
    return s.to_string().is_ok_and(|s| s.is_empty());
  }
  false
}
