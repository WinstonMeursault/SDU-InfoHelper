//! Contract only: authenticate the transport BEFORE calling actor_from_event.
//! HTTP/WS connections, access-token validation, binding pages and friend approval
//! are intentionally left for the NapCat integration phase.
use super::{Actor, Delivery, ServiceError};
use serde_json::{Value, json};

pub fn actor_from_event(event: &Value, expected_bot: &str) -> Result<Actor, ServiceError> {
    if event.get("post_type").and_then(Value::as_str) != Some("message")
        || event.get("message_type").and_then(Value::as_str) != Some("private")
        || event.get("sub_type").and_then(Value::as_str) != Some("friend")
    {
        return Err(ServiceError::Invalid("仅接受好友私聊消息。"));
    }
    fn id(value: Option<&Value>) -> Result<String, ServiceError> {
        match value {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(Value::Number(n)) => n
                .as_u64()
                .map(|n| n.to_string())
                .ok_or(ServiceError::Invalid("QQ 身份字段无效。")),
            _ => Err(ServiceError::Invalid("QQ 身份字段缺失。")),
        }
    }
    let bot = id(event.get("self_id"))?;
    if bot != expected_bot {
        return Err(ServiceError::Invalid("事件来自其他机器人。"));
    }
    Actor::qq(&bot, &id(event.get("user_id"))?)
}

/// Plain text segments avoid interpreting dorm text as CQ code. The recipient
/// comes from the tenant registry, not from the user-supplied command body.
pub fn private_message(delivery: &Delivery) -> Value {
    json!({"action":"send_private_msg", "params":{"user_id":delivery.actor.user_id(),"message":[{"type":"text","data":{"text":delivery.message}}]},"echo":delivery.notification_id})
}
