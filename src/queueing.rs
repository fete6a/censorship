use serde_json::Value;
use worker::{Env, QueueContentType, RawMessageBuilder, Result};

/// The SDK's default serializer turns Value maps into JS Map, which JSON drops.
pub async fn send(env: &Env, binding: &str, value: &Value) -> Result<()> {
    let body = js_sys::JSON::parse(&serde_json::to_string(value)?)?;
    env.queue(binding)?.send_raw(
        RawMessageBuilder::new(body).build_with_content_type(QueueContentType::Json),
    ).await
}
