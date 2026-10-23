use js_sys::{futures::JsFuture, Function, JSON, Promise, Reflect};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use worker::{Env, Result};
use crate::util::fail;

/// Preserve JSON objects at the JS binding boundary; the SDK uses JS Map for Value.
pub async fn run(env: &Env, model: &str, input: &Value) -> Result<Value> {
    let ai = env.ai("AI")?;
    let binding: &JsValue = ai.as_ref();
    let run = Reflect::get(binding, &JsValue::from_str("run"))?
        .dyn_into::<Function>().map_err(|_| fail("AI binding has no run function"))?;
    let input = JSON::parse(&serde_json::to_string(input)?)?;
    let future = run.call2(binding, &JsValue::from_str(model), &input)?;
    let output = JsFuture::from(Promise::resolve(&future)).await?;
    let json = JSON::stringify(&output)?.as_string()
        .ok_or_else(|| fail("AI returned no JSON response"))?;
    Ok(serde_json::from_str(&json)?)
}
