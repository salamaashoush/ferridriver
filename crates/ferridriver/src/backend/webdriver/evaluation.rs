use serde_json::{Value, json};

use super::page::WebDriverPage;
use crate::error::{FerriError, Result};
use crate::js_handle::{EvaluateResult, HandleRemote, JSHandleBacking};
use crate::protocol::{HandleId, SerializedValue, SpecialValue};

impl WebDriverPage {
  pub async fn call_utility_evaluate(
    &self,
    source: &str,
    args: &[SerializedValue],
    handles: &[HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<EvaluateResult> {
    let frame = frame_id.or_else(|| {
      handles.iter().find_map(|handle| match handle {
        HandleId::WebDriver { frame, .. } => Some(frame.as_str()),
        _ => None,
      })
    });
    let page = self.in_frame(frame)?;
    page.ensure_engine_injected().await?;
    let frame = page.frame_id();
    let handles = handles
      .iter()
      .map(|handle| {
        let HandleId::WebDriver {
          id,
          frame: handle_frame,
          element,
        } = handle
        else {
          return Err(FerriError::invalid_argument(
            "handle",
            "handle belongs to another backend",
          ));
        };
        if handle_frame != &frame {
          return Err(FerriError::invalid_argument(
            "handle",
            "handle belongs to another frame",
          ));
        }
        Ok(if *element {
          json!({"element":true,"value":{"element-6066-11e4-a52e-4f735466cecf":id}})
        } else {
          json!({"element":false,"id":id})
        })
      })
      .collect::<Result<Vec<_>>>()?;
    let script = format!(
      r"
const [isFn,byValue,expression,count,serialized,refs]=arguments;
const handles=refs.map(ref=>{{
  if(ref.element) return ref.value;
  const map=window.__fd.__webdriverHandles;
  if(!map || !map.has(ref.id)) throw new Error('WebDriver handle was disposed or its document was replaced');
  return map.get(ref.id);
}});
const result=({})(isFn,byValue,expression,count,serialized,...handles);
return Promise.resolve(result).then(value=>{{
  if(byValue) return {{kind:'value',value}};
  if(value!==null && (typeof value==='object' || typeof value==='function')) {{
    if(value.nodeType===1) return {{kind:'element',value}};
    const map=window.__fd.__webdriverHandles ||= new Map();
    const id=Array.from(crypto.getRandomValues(new Uint32Array(4)),n=>n.toString(16)).join('-');
    map.set(id,value);
    return {{kind:'object',id}};
  }}
  const encoded=JSON.stringify(window.__fd.__us.jsonValue(true,value));
  return {{kind:'value',value:encoded===undefined?null:encoded}};
}});
",
      crate::backend::cdp::UTILITY_EVAL_WRAPPER
    );
    let result = page
      .execute_script(
        &script,
        vec![
          json!(is_function),
          json!(return_by_value),
          json!(source),
          json!(args.len()),
          json!(serde_json::to_string(args)?),
          json!(handles),
        ],
      )
      .await?;
    evaluation_result(&result, frame, return_by_value)
  }

  pub async fn release_handle(&self, id: &str, frame: &str, element: bool) -> Result<()> {
    // Classic element references are browser-owned and have no release endpoint.
    if !element {
      self
        .in_frame(Some(frame))?
        .execute_script(
          "window.__fd?.__webdriverHandles?.delete(arguments[0]);",
          vec![json!(id)],
        )
        .await?;
    }
    Ok(())
  }
}

fn evaluation_result(result: &Value, frame: String, by_value: bool) -> Result<EvaluateResult> {
  match result["kind"].as_str() {
    Some("value") => {
      let value = match &result["value"] {
        Value::Null => SerializedValue::Special(SpecialValue::Undefined),
        Value::String(value) => {
          serde_json::from_str(value).map_err(|error| FerriError::protocol("evaluate", error.to_string()))?
        },
        _ => return Err(FerriError::protocol("evaluate", "invalid serialized value")),
      };
      Ok(if by_value {
        EvaluateResult::Value(value)
      } else {
        EvaluateResult::Handle(JSHandleBacking::Value(value), false)
      })
    },
    Some(kind @ ("element" | "object")) => {
      let element = kind == "element";
      let id = if element {
        &result["value"]["element-6066-11e4-a52e-4f735466cecf"]
      } else {
        &result["id"]
      };
      let id = id
        .as_str()
        .ok_or_else(|| FerriError::protocol("evaluate handle", "missing object reference"))?
        .to_owned();
      Ok(EvaluateResult::Handle(
        JSHandleBacking::Remote(HandleRemote::WebDriver { id, frame, element }),
        element,
      ))
    },
    _ => Err(FerriError::protocol("evaluate", "invalid utility response")),
  }
}
